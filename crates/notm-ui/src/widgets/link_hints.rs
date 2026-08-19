use std::{cell::RefCell, rc::Rc, time::Duration};

use gtk::prelude::*;
use gtk4 as gtk;
use serde::{Deserialize, Serialize};
use webkit6::{LoadEvent, prelude::WebViewExt};

const LINK_HINT_ALPHABET: &str = "asdfghjklqwertyuiopzxcvbnm";
const LINK_HINT_WORLD: &str = "notm-link-hints";
const LINK_HINT_SOURCE_URI: &str = "notm://link-hints";

const COLLECT_VISIBLE_LINKS_SCRIPT: &str = r#"
(() => {
  document.getElementById("notm-link-hints-overlay")?.remove();
  const supportedSchemes = new Set(["http:", "https:", "mailto:"]);
  const targets = [];
  let eligibleCount = 0;
  let laidOutCount = 0;
  for (const anchor of document.querySelectorAll("a[href]")) {
    let target;
    try {
      target = new URL(anchor.href);
    } catch (_) {
      continue;
    }
    if (!supportedSchemes.has(target.protocol.toLowerCase())) {
      continue;
    }
    const style = window.getComputedStyle(anchor);
    if (style.display === "none" || style.visibility === "hidden" || Number(style.opacity) === 0) {
      continue;
    }
    eligibleCount += 1;
    const laidOutRects = Array.from(anchor.getClientRects()).filter((candidate) =>
      candidate.width > 0 && candidate.height > 0
    );
    if (laidOutRects.length > 0) {
      laidOutCount += 1;
    }
    const rect = laidOutRects.find((candidate) =>
      candidate.right > 0 &&
      candidate.bottom > 0 &&
      candidate.left < window.innerWidth &&
      candidate.top < window.innerHeight
    );
    if (!rect) {
      continue;
    }
    targets.push({
      uri: anchor.href,
      x: Math.max(2, Math.min(rect.left, window.innerWidth - 2)),
      y: Math.max(2, Math.min(rect.top, window.innerHeight - 2))
    });
  }
  return JSON.stringify({
    targets,
    eligible_count: eligibleCount,
    laid_out_count: laidOutCount
  });
})()
"#;

const RENDER_LINK_HINTS_SCRIPT: &str = r##"
(() => {
  document.getElementById("notm-link-hints-overlay")?.remove();
  const hints = __NOTM_LINK_HINTS__;
  const overlay = document.createElement("div");
  overlay.id = "notm-link-hints-overlay";
  overlay.setAttribute("aria-hidden", "true");
  Object.assign(overlay.style, {
    position: "fixed",
    inset: "0",
    width: "100vw",
    height: "100vh",
    overflow: "hidden",
    pointerEvents: "none",
    zIndex: "2147483647"
  });
  for (const hint of hints) {
    const label = document.createElement("span");
    label.dataset.notmLinkHint = hint.label;
    label.textContent = hint.label.toUpperCase();
    Object.assign(label.style, {
      position: "absolute",
      left: `${hint.x}px`,
      top: `${hint.y}px`,
      transform: "translate(-2px, -2px)",
      display: "block",
      padding: "2px 3px",
      border: "1px solid #5f4b00",
      borderRadius: "3px",
      boxShadow: "0 1px 3px rgba(0, 0, 0, 0.45)",
      background: "#ffd75f",
      color: "#111111",
      font: "700 12px/1 ui-monospace, monospace",
      letterSpacing: "0.04em",
      whiteSpace: "nowrap"
    });
    overlay.appendChild(label);
  }
  document.documentElement.appendChild(overlay);
  return JSON.stringify({count: overlay.childElementCount});
})()
"##;

const REMOVE_LINK_HINTS_SCRIPT: &str = r#"
(() => {
  document.getElementById("notm-link-hints-overlay")?.remove();
})()
"#;

const FILTER_LINK_HINTS_SCRIPT: &str = r#"
(() => {
  const overlay = document.getElementById("notm-link-hints-overlay");
  if (!overlay) {
    return JSON.stringify({count: 0});
  }
  const prefix = __NOTM_LINK_HINT_PREFIX__;
  let count = 0;
  for (const label of overlay.querySelectorAll("[data-notm-link-hint]")) {
    const matches = label.dataset.notmLinkHint.startsWith(prefix);
    label.style.display = matches ? "block" : "none";
    if (matches) {
      count += 1;
    }
  }
  return JSON.stringify({count});
})()
"#;

pub(crate) type LinkHintOpener = Rc<dyn Fn(&str, &gtk::Label)>;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct LinkHintSnapshot {
    pub(crate) phase: &'static str,
    pub(crate) active: bool,
    pub(crate) loading: bool,
    pub(crate) prefix: String,
    pub(crate) labels: Vec<String>,
    pub(crate) candidate_count: usize,
    pub(crate) overlay_count: usize,
}

#[derive(Clone)]
pub(crate) struct LinkHintController(Rc<LinkHintControllerInner>);

struct LinkHintControllerInner {
    view: webkit6::WebView,
    status_label: gtk::Label,
    opener: LinkHintOpener,
    state: RefCell<LinkHintState>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum LinkHintPhase {
    #[default]
    Idle,
    AwaitingLoad,
    Collecting,
    Rendering,
    Active,
}

impl LinkHintPhase {
    fn name(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::AwaitingLoad => "awaiting_load",
            Self::Collecting => "collecting",
            Self::Rendering => "rendering",
            Self::Active => "active",
        }
    }

    fn is_loading(self) -> bool {
        matches!(
            self,
            Self::AwaitingLoad | Self::Collecting | Self::Rendering
        )
    }

    fn consumes_keys(self) -> bool {
        self != Self::Idle
    }
}

#[derive(Default)]
struct LinkHintState {
    phase: LinkHintPhase,
    generation: u64,
    prefix: String,
    candidates: Vec<LinkHintCandidate>,
    overlay_count: usize,
    collect_attempts: u8,
}

#[derive(Debug, Clone)]
struct LinkHintCandidate {
    label: String,
    uri: String,
}

#[derive(Debug, Deserialize)]
struct VisibleLinkTarget {
    uri: String,
    x: f64,
    y: f64,
}

#[derive(Debug, Deserialize)]
struct VisibleLinkCollection {
    targets: Vec<VisibleLinkTarget>,
    eligible_count: usize,
    laid_out_count: usize,
}

#[derive(Serialize)]
struct LinkHintOverlay<'a> {
    label: &'a str,
    x: f64,
    y: f64,
}

#[derive(Debug, Deserialize)]
struct RenderedLinkHints {
    count: usize,
}

impl LinkHintController {
    pub(crate) fn new(
        view: &webkit6::WebView,
        status_label: &gtk::Label,
        opener: LinkHintOpener,
    ) -> Self {
        let controller = Self(Rc::new(LinkHintControllerInner {
            view: view.clone(),
            status_label: status_label.clone(),
            opener,
            state: RefCell::new(LinkHintState::default()),
        }));
        Self::connect_load_lifecycle(&controller.0);
        controller
    }

    pub(crate) fn start(&self) {
        let generation = {
            let mut state = self.0.state.borrow_mut();
            state.generation = state.generation.wrapping_add(1);
            state.prefix.clear();
            state.candidates.clear();
            state.overlay_count = 0;
            state.collect_attempts = 0;
            state.phase = if self.0.view.is_loading() {
                LinkHintPhase::AwaitingLoad
            } else {
                LinkHintPhase::Collecting
            };
            state.generation
        };
        self.0.status_label.set_text(if self.0.view.is_loading() {
            "Link hints: waiting for Visual HTML to finish loading…"
        } else {
            "Link hints: finding visible links…"
        });
        self.0.remove_overlays();
        if !self.0.view.is_loading() {
            LinkHintControllerInner::collect_visible_links(&self.0, generation);
        }
    }

    pub(crate) fn cancel(&self) {
        if self.0.reset() {
            self.0.status_label.set_text("Link hints cancelled");
        }
        self.0.remove_overlays();
    }

    pub(crate) fn cancel_silent(&self) {
        self.0.reset();
        self.0.remove_overlays();
    }

    pub(crate) fn handle_key(&self, key: gtk::gdk::Key, modifiers: gtk::gdk::ModifierType) -> bool {
        if !self.0.state.borrow().phase.consumes_keys() {
            return false;
        }
        if key == gtk::gdk::Key::Escape {
            self.cancel();
            return true;
        }
        if key == gtk::gdk::Key::BackSpace {
            self.backspace();
            return true;
        }
        if modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK)
            || modifiers.contains(gtk::gdk::ModifierType::ALT_MASK)
            || modifiers.contains(gtk::gdk::ModifierType::SUPER_MASK)
        {
            self.0
                .status_label
                .set_text("Link hints: type a displayed letter, Backspace, or Esc");
            return true;
        }
        if let Some(key) = key.to_unicode() {
            self.input_char(key)
        } else {
            self.0
                .status_label
                .set_text("Link hints: type a displayed letter, Backspace, or Esc");
        }
        true
    }

    pub(crate) fn input_char(&self, input: char) {
        let input = input.to_ascii_lowercase();
        if !LINK_HINT_ALPHABET.contains(input) {
            self.0
                .status_label
                .set_text("Link hints: type a displayed letter, Backspace, or Esc");
            return;
        }

        let outcome = {
            let mut state = self.0.state.borrow_mut();
            if state.phase.is_loading() {
                LinkHintInputOutcome::Loading
            } else if state.phase != LinkHintPhase::Active {
                LinkHintInputOutcome::Inactive
            } else {
                apply_link_hint_char(&mut state, input)
            }
        };
        self.0.apply_input_outcome(outcome);
    }

    pub(crate) fn snapshot(&self) -> LinkHintSnapshot {
        let state = self.0.state.borrow();
        LinkHintSnapshot {
            phase: state.phase.name(),
            active: state.phase == LinkHintPhase::Active,
            loading: state.phase.is_loading(),
            prefix: state.prefix.clone(),
            labels: state
                .candidates
                .iter()
                .map(|candidate| candidate.label.clone())
                .collect(),
            candidate_count: state.candidates.len(),
            overlay_count: state.overlay_count,
        }
    }

    fn backspace(&self) {
        let update = {
            let mut state = self.0.state.borrow_mut();
            if state.phase.is_loading() {
                None
            } else if state.phase == LinkHintPhase::Active {
                state.prefix.pop();
                let matches = matching_hint_count(&state.candidates, &state.prefix);
                Some((state.prefix.clone(), matches, state.candidates.len()))
            } else {
                return;
            }
        };
        let Some((prefix, matches, total)) = update else {
            self.0
                .status_label
                .set_text("Link hints are still loading…");
            return;
        };
        self.0.filter_overlays(&prefix);
        self.0
            .status_label
            .set_text(&link_hint_prompt(&prefix, matches, total));
    }

    fn connect_load_lifecycle(inner: &Rc<LinkHintControllerInner>) {
        let weak = Rc::downgrade(inner);
        inner.view.connect_load_changed(move |_, event| {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            match event {
                LoadEvent::Started => {
                    let mut state = inner.state.borrow_mut();
                    let awaiting = state.phase == LinkHintPhase::AwaitingLoad;
                    state.generation = state.generation.wrapping_add(1);
                    state.prefix.clear();
                    state.candidates.clear();
                    state.overlay_count = 0;
                    state.collect_attempts = 0;
                    state.phase = if awaiting {
                        LinkHintPhase::AwaitingLoad
                    } else {
                        LinkHintPhase::Idle
                    };
                }
                LoadEvent::Finished => {
                    let generation = {
                        let mut state = inner.state.borrow_mut();
                        if state.phase != LinkHintPhase::AwaitingLoad {
                            return;
                        }
                        state.phase = LinkHintPhase::Collecting;
                        state.generation
                    };
                    inner
                        .status_label
                        .set_text("Link hints: finding visible links…");
                    LinkHintControllerInner::collect_visible_links(&inner, generation);
                }
                _ => {}
            }
        });

        let weak = Rc::downgrade(inner);
        inner.view.connect_unmap(move |_| {
            if let Some(inner) = weak.upgrade() {
                inner.reset();
                inner.remove_overlays();
            }
        });
    }
}

impl LinkHintControllerInner {
    fn collect_visible_links(inner: &Rc<Self>, generation: u64) {
        let weak = Rc::downgrade(inner);
        inner.view.evaluate_javascript(
            COLLECT_VISIBLE_LINKS_SCRIPT,
            Some(LINK_HINT_WORLD),
            Some(LINK_HINT_SOURCE_URI),
            None::<&gtk::gio::Cancellable>,
            move |result| {
                let parsed = result.map_err(|error| error.to_string()).and_then(|value| {
                    serde_json::from_str::<VisibleLinkCollection>(&value.to_str())
                        .map_err(|error| error.to_string())
                });
                if let Some(inner) = weak.upgrade() {
                    Self::finish_collecting(&inner, generation, parsed);
                }
            },
        );
    }

    fn finish_collecting(
        inner: &Rc<Self>,
        generation: u64,
        result: Result<VisibleLinkCollection, String>,
    ) {
        let collection = match result {
            Ok(collection) => collection,
            Err(error) => {
                inner.fail(generation, &format!("Link hints failed: {error}"));
                return;
            }
        };
        let retry_layout = collection.targets.is_empty()
            && collection.eligible_count > 0
            && collection.laid_out_count == 0;
        let targets = collection
            .targets
            .into_iter()
            .filter(|target| {
                target.x.is_finite()
                    && target.y.is_finite()
                    && html_link_scheme_is_external_safe(&target.uri)
            })
            .collect::<Vec<_>>();
        if targets.is_empty() {
            let retry = {
                let mut state = inner.state.borrow_mut();
                if retry_layout
                    && state.generation == generation
                    && state.phase == LinkHintPhase::Collecting
                    && state.collect_attempts < 10
                {
                    state.collect_attempts += 1;
                    true
                } else {
                    false
                }
            };
            if retry {
                inner
                    .status_label
                    .set_text("Link hints: waiting for the HTML layout…");
                let weak = Rc::downgrade(inner);
                gtk::glib::timeout_add_local_once(Duration::from_millis(100), move || {
                    if let Some(inner) = weak.upgrade()
                        && inner.generation_is_current(generation)
                        && inner.state.borrow().phase == LinkHintPhase::Collecting
                    {
                        Self::collect_visible_links(&inner, generation);
                    }
                });
                return;
            }
            if inner.generation_is_current(generation) {
                inner.reset();
                inner
                    .status_label
                    .set_text("No visible links in this HTML message");
            }
            return;
        }

        let labels = link_hint_labels(targets.len());
        let candidates = targets
            .iter()
            .zip(&labels)
            .map(|(target, label)| LinkHintCandidate {
                label: label.clone(),
                uri: target.uri.clone(),
            })
            .collect::<Vec<_>>();
        let overlays = targets
            .iter()
            .zip(&labels)
            .map(|(target, label)| LinkHintOverlay {
                label,
                x: target.x,
                y: target.y,
            })
            .collect::<Vec<_>>();
        let script = match serde_json::to_string(&overlays) {
            Ok(overlays) => RENDER_LINK_HINTS_SCRIPT.replace("__NOTM_LINK_HINTS__", &overlays),
            Err(error) => {
                inner.fail(generation, &format!("Link hints failed: {error}"));
                return;
            }
        };
        {
            let mut state = inner.state.borrow_mut();
            if state.generation != generation || state.phase != LinkHintPhase::Collecting {
                return;
            }
            state.phase = LinkHintPhase::Rendering;
            state.candidates = candidates;
        }

        let expected_count = overlays.len();
        let weak = Rc::downgrade(inner);
        inner.view.evaluate_javascript(
            &script,
            Some(LINK_HINT_WORLD),
            Some(LINK_HINT_SOURCE_URI),
            None::<&gtk::gio::Cancellable>,
            move |result| {
                let parsed = result.map_err(|error| error.to_string()).and_then(|value| {
                    serde_json::from_str::<RenderedLinkHints>(&value.to_str())
                        .map_err(|error| error.to_string())
                });
                if let Some(inner) = weak.upgrade() {
                    inner.finish_rendering(generation, expected_count, parsed);
                }
            },
        );
    }

    fn finish_rendering(
        &self,
        generation: u64,
        expected_count: usize,
        result: Result<RenderedLinkHints, String>,
    ) {
        let rendered = match result {
            Ok(rendered) if rendered.count == expected_count => rendered,
            Ok(rendered) => {
                self.fail(
                    generation,
                    &format!(
                        "Link hints failed: rendered {} of {expected_count} labels",
                        rendered.count
                    ),
                );
                return;
            }
            Err(error) => {
                self.fail(generation, &format!("Link hints failed: {error}"));
                return;
            }
        };
        let (prefix, total) = {
            let mut state = self.state.borrow_mut();
            if state.generation != generation || state.phase != LinkHintPhase::Rendering {
                return;
            }
            state.phase = LinkHintPhase::Active;
            state.overlay_count = rendered.count;
            (state.prefix.clone(), state.candidates.len())
        };
        self.status_label
            .set_text(&link_hint_prompt(&prefix, total, total));
    }

    fn apply_input_outcome(&self, outcome: LinkHintInputOutcome) {
        match outcome {
            LinkHintInputOutcome::Inactive => {}
            LinkHintInputOutcome::Loading => {
                self.status_label.set_text("Link hints are still loading…")
            }
            LinkHintInputOutcome::Invalid {
                attempted,
                prefix,
                matches,
                total,
            } => self.status_label.set_text(&format!(
                "No link hint starts with {}; {}",
                attempted.to_ascii_uppercase(),
                link_hint_prompt(&prefix, matches, total).to_ascii_lowercase()
            )),
            LinkHintInputOutcome::Updated {
                prefix,
                matches,
                total,
            } => {
                self.filter_overlays(&prefix);
                self.status_label
                    .set_text(&link_hint_prompt(&prefix, matches, total));
            }
            LinkHintInputOutcome::Selected { uri } => {
                self.remove_overlays();
                (self.opener)(&uri, &self.status_label);
            }
        }
    }

    fn filter_overlays(&self, prefix: &str) {
        let prefix = match serde_json::to_string(prefix) {
            Ok(prefix) => prefix,
            Err(_) => return,
        };
        let script = FILTER_LINK_HINTS_SCRIPT.replace("__NOTM_LINK_HINT_PREFIX__", &prefix);
        self.view.evaluate_javascript(
            &script,
            Some(LINK_HINT_WORLD),
            Some(LINK_HINT_SOURCE_URI),
            None::<&gtk::gio::Cancellable>,
            |_| {},
        );
    }

    fn remove_overlays(&self) {
        self.view.evaluate_javascript(
            REMOVE_LINK_HINTS_SCRIPT,
            Some(LINK_HINT_WORLD),
            Some(LINK_HINT_SOURCE_URI),
            None::<&gtk::gio::Cancellable>,
            |_| {},
        );
    }

    fn fail(&self, generation: u64, message: &str) {
        if !self.generation_is_current(generation) {
            return;
        }
        self.reset();
        self.remove_overlays();
        self.status_label.set_text(message);
    }

    fn generation_is_current(&self, generation: u64) -> bool {
        self.state.borrow().generation == generation
    }

    fn reset(&self) -> bool {
        let mut state = self.state.borrow_mut();
        let was_active = state.phase.consumes_keys();
        state.generation = state.generation.wrapping_add(1);
        state.phase = LinkHintPhase::Idle;
        state.prefix.clear();
        state.candidates.clear();
        state.overlay_count = 0;
        state.collect_attempts = 0;
        was_active
    }
}

#[derive(Debug, PartialEq, Eq)]
enum LinkHintInputOutcome {
    Inactive,
    Loading,
    Invalid {
        attempted: String,
        prefix: String,
        matches: usize,
        total: usize,
    },
    Updated {
        prefix: String,
        matches: usize,
        total: usize,
    },
    Selected {
        uri: String,
    },
}

fn apply_link_hint_char(state: &mut LinkHintState, input: char) -> LinkHintInputOutcome {
    let attempted = format!("{}{input}", state.prefix);
    let matching = state
        .candidates
        .iter()
        .filter(|candidate| candidate.label.starts_with(&attempted))
        .collect::<Vec<_>>();
    if matching.is_empty() {
        return LinkHintInputOutcome::Invalid {
            attempted,
            prefix: state.prefix.clone(),
            matches: matching_hint_count(&state.candidates, &state.prefix),
            total: state.candidates.len(),
        };
    }
    if let Some(candidate) = matching
        .iter()
        .find(|candidate| candidate.label == attempted)
    {
        let uri = candidate.uri.clone();
        state.generation = state.generation.wrapping_add(1);
        state.phase = LinkHintPhase::Idle;
        state.prefix.clear();
        state.candidates.clear();
        state.overlay_count = 0;
        state.collect_attempts = 0;
        return LinkHintInputOutcome::Selected { uri };
    }
    state.prefix = attempted;
    LinkHintInputOutcome::Updated {
        prefix: state.prefix.clone(),
        matches: matching.len(),
        total: state.candidates.len(),
    }
}

fn matching_hint_count(candidates: &[LinkHintCandidate], prefix: &str) -> usize {
    candidates
        .iter()
        .filter(|candidate| candidate.label.starts_with(prefix))
        .count()
}

fn link_hint_prompt(prefix: &str, matches: usize, total: usize) -> String {
    if prefix.is_empty() {
        format!("Link hints: type a displayed label ({total} visible); Esc cancels")
    } else {
        format!(
            "Link hints: {} ({matches} match{}); Backspace edits, Esc cancels",
            prefix.to_ascii_uppercase(),
            if matches == 1 { "" } else { "es" }
        )
    }
}

fn link_hint_labels(count: usize) -> Vec<String> {
    if count == 0 {
        return Vec::new();
    }
    let alphabet = LINK_HINT_ALPHABET.as_bytes();
    let width = hint_label_width(count, alphabet.len());
    (0..count)
        .map(|index| hint_label(index, width, alphabet))
        .collect()
}

fn hint_label_width(count: usize, radix: usize) -> usize {
    let mut width = 1;
    let mut capacity = radix;
    while capacity < count {
        width += 1;
        capacity = capacity.saturating_mul(radix);
    }
    width
}

fn hint_label(mut index: usize, width: usize, alphabet: &[u8]) -> String {
    let mut label = vec![alphabet[0]; width];
    for position in (0..width).rev() {
        label[position] = alphabet[index % alphabet.len()];
        index /= alphabet.len();
    }
    String::from_utf8(label).expect("link hint alphabet is ASCII")
}

pub(crate) fn html_link_scheme_is_external_safe(uri: &str) -> bool {
    let Some((scheme, _)) = uri.split_once(':') else {
        return false;
    };
    matches!(
        scheme.to_ascii_lowercase().as_str(),
        "http" | "https" | "mailto"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active_state(count: usize) -> LinkHintState {
        LinkHintState {
            phase: LinkHintPhase::Active,
            candidates: link_hint_labels(count)
                .into_iter()
                .enumerate()
                .map(|(index, label)| LinkHintCandidate {
                    label,
                    uri: format!("https://example.test/{index}"),
                })
                .collect(),
            overlay_count: count,
            ..LinkHintState::default()
        }
    }

    #[test]
    fn labels_are_single_keys_until_the_alphabet_is_exhausted() {
        let labels = link_hint_labels(LINK_HINT_ALPHABET.len());
        assert_eq!(labels.len(), LINK_HINT_ALPHABET.len());
        assert!(labels.iter().all(|label| label.len() == 1));
        assert_eq!(labels.first().map(String::as_str), Some("a"));
        assert_eq!(labels.last().map(String::as_str), Some("m"));
    }

    #[test]
    fn labels_expand_to_a_fixed_prefix_free_width() {
        let labels = link_hint_labels(LINK_HINT_ALPHABET.len() + 1);
        assert!(labels.iter().all(|label| label.len() == 2));
        for (index, label) in labels.iter().enumerate() {
            assert!(
                labels
                    .iter()
                    .enumerate()
                    .all(|(other_index, other)| index == other_index || !other.starts_with(label)),
                "{label} was a prefix of another hint"
            );
        }
    }

    #[test]
    fn every_single_key_hint_is_selectable_including_application_bindings() {
        for (index, input) in LINK_HINT_ALPHABET.chars().enumerate() {
            let mut state = active_state(LINK_HINT_ALPHABET.len());
            assert_eq!(
                apply_link_hint_char(&mut state, input),
                LinkHintInputOutcome::Selected {
                    uri: format!("https://example.test/{index}"),
                },
                "link hint {input} was not selectable"
            );
        }
    }

    #[test]
    fn input_filters_then_selects_the_exact_link() {
        let mut state = active_state(LINK_HINT_ALPHABET.len() + 1);
        let expected_label = state.candidates[1].label.clone();
        let expected_uri = state.candidates[1].uri.clone();

        let first = apply_link_hint_char(&mut state, expected_label.as_bytes()[0] as char);
        assert!(matches!(first, LinkHintInputOutcome::Updated { .. }));
        assert_eq!(state.prefix.len(), 1);

        let second = apply_link_hint_char(&mut state, expected_label.as_bytes()[1] as char);
        assert_eq!(second, LinkHintInputOutcome::Selected { uri: expected_uri });
        assert_eq!(state.phase, LinkHintPhase::Idle);
        assert!(state.candidates.is_empty());
    }

    #[test]
    fn invalid_input_preserves_the_valid_prefix() {
        let mut state = active_state(LINK_HINT_ALPHABET.len() + 1);
        let first = state.candidates.last().expect("last hint").label.as_bytes()[0] as char;
        let _ = apply_link_hint_char(&mut state, first);
        let prefix = state.prefix.clone();

        let invalid = apply_link_hint_char(&mut state, 'm');
        assert!(matches!(invalid, LinkHintInputOutcome::Invalid { .. }));
        assert_eq!(state.prefix, prefix);
        assert_eq!(state.phase, LinkHintPhase::Active);
    }

    #[test]
    fn only_external_link_schemes_are_hintable() {
        assert!(html_link_scheme_is_external_safe("https://example.test"));
        assert!(html_link_scheme_is_external_safe(
            "MAILTO:user@example.test"
        ));
        assert!(!html_link_scheme_is_external_safe("javascript:alert(1)"));
        assert!(!html_link_scheme_is_external_safe("/relative"));
    }
}
