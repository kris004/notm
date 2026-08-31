use std::{
    cell::RefCell,
    collections::BTreeMap,
    rc::{Rc, Weak},
};

use gtk4 as gtk;
use serde::{Deserialize, Serialize};
use webkit6::prelude::WebViewExt;

const SCROLL_MESSAGE_HANDLER: &str = "notm_html_scroll";

const SCROLL_OBSERVER_SCRIPT: &str = r#"
(() => {
  const handler = window.webkit?.messageHandlers?.notm_html_scroll;
  if (!handler) return;
  let queued = false;
  const imageMetrics = () => {
    const images = Array.from(document.images || []);
    const loaded = images.filter(image => image.complete && image.naturalWidth > 0).length;
    const failed = images.filter(image => image.complete && image.naturalWidth === 0).length;
    return {
      total: images.length,
      loaded,
      failed,
      pending: images.length - loaded - failed
    };
  };
  const report = () => {
    queued = false;
    const e = document.scrollingElement || document.documentElement || document.body;
    handler.postMessage(JSON.stringify({
      generation: Number(document.documentElement?.dataset?.notmGeneration || "0"),
      ready: document.readyState === "complete",
      y: e?.scrollTop || 0,
      h: e?.scrollHeight || 0,
      c: e?.clientHeight || 0,
      images: imageMetrics()
    }));
  };
  const queueReport = () => {
    if (queued) return;
    queued = true;
    requestAnimationFrame(report);
  };
  document.addEventListener("scroll", queueReport, {capture: true, passive: true});
  window.addEventListener("resize", queueReport, {passive: true});
  window.addEventListener("load", report, {once: true});
  report();
})();
"#;

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub(crate) struct HtmlScrollMetrics {
    pub(crate) y: f64,
    pub(crate) h: f64,
    pub(crate) c: f64,
    #[serde(rename = "canScroll")]
    pub(crate) can_scroll: bool,
    pub(crate) fraction: f64,
}

impl HtmlScrollMetrics {
    fn from_report(report: ScrollReport) -> Self {
        let h = report.h.max(0.0);
        let c = report.c.max(0.0);
        let max = (h - c).max(0.0);
        let y = report.y.clamp(0.0, max);
        Self {
            y,
            h,
            c,
            can_scroll: max > 0.0,
            fraction: if max > 0.0 {
                (y / max).clamp(0.0, 1.0)
            } else {
                0.0
            },
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct HtmlImageMetrics {
    pub(crate) total: u64,
    pub(crate) loaded: u64,
    pub(crate) failed: u64,
    pub(crate) pending: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct HtmlViewLifecycleSnapshot {
    pub(crate) generation: u64,
    pub(crate) completed_generation: u64,
    pub(crate) ready: bool,
    #[serde(rename = "pending")]
    pub(crate) evaluation_pending: bool,
    pub(crate) pending_restore: Option<f64>,
    pub(crate) scroll: Option<HtmlScrollMetrics>,
    pub(crate) images: Option<HtmlImageMetrics>,
    #[serde(rename = "error")]
    pub(crate) last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
struct ScrollReport {
    generation: u64,
    ready: bool,
    y: f64,
    h: f64,
    c: f64,
    #[serde(default)]
    images: HtmlImageMetrics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EvaluationKind {
    Probe,
    Scroll,
    Restore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Evaluation {
    id: u64,
    generation: u64,
    kind: EvaluationKind,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct PendingRestore {
    generation: u64,
    fraction: f64,
}

#[derive(Debug, Default)]
struct LifecycleState {
    generation: u64,
    completed_generation: u64,
    ready_generation: Option<u64>,
    metrics: Option<HtmlScrollMetrics>,
    image_metrics: Option<HtmlImageMetrics>,
    pending_restore: Option<PendingRestore>,
    restore_in_flight: Option<u64>,
    next_evaluation_id: u64,
    evaluations: BTreeMap<u64, Evaluation>,
    last_error: Option<String>,
}

impl LifecycleState {
    fn begin_load(&mut self) -> u64 {
        self.generation = self.generation.checked_add(1).unwrap_or(1);
        self.ready_generation = None;
        self.metrics = None;
        self.image_metrics = None;
        self.pending_restore = None;
        self.restore_in_flight = None;
        // Completion callbacks retain their own generation token, so keeping
        // superseded evaluations in the live-state map serves no purpose and
        // would let rapid document replacement grow it until WebKit replies.
        self.evaluations.clear();
        self.last_error = None;
        self.generation
    }

    fn begin_evaluation(&mut self, kind: EvaluationKind) -> Evaluation {
        self.next_evaluation_id = self.next_evaluation_id.checked_add(1).unwrap_or(1);
        let evaluation = Evaluation {
            id: self.next_evaluation_id,
            generation: self.generation,
            kind,
        };
        self.evaluations.insert(evaluation.id, evaluation);
        evaluation
    }

    fn queue_restore(&mut self, fraction: f64) {
        self.pending_restore = Some(PendingRestore {
            generation: self.generation,
            fraction: fraction.clamp(0.0, 1.0),
        });
    }

    fn begin_ready_restore(&mut self) -> Option<(Evaluation, f64)> {
        let pending = self.pending_restore?;
        if pending.generation != self.generation
            || self.ready_generation != Some(self.generation)
            || self.restore_in_flight.is_some()
        {
            return None;
        }
        let evaluation = self.begin_evaluation(EvaluationKind::Restore);
        self.restore_in_flight = Some(evaluation.id);
        Some((evaluation, pending.fraction))
    }

    fn record_report(&mut self, report: ScrollReport) -> bool {
        if report.generation != self.generation {
            return false;
        }
        self.ready_generation = report.ready.then_some(report.generation);
        if report.ready {
            self.completed_generation = report.generation;
        }
        self.metrics = Some(HtmlScrollMetrics::from_report(report));
        self.image_metrics = Some(report.images);
        self.last_error = None;
        true
    }

    fn complete_evaluation(
        &mut self,
        evaluation: Evaluation,
        result: Result<ScrollReport, String>,
    ) -> bool {
        self.evaluations.remove(&evaluation.id);
        if evaluation.generation != self.generation {
            return false;
        }
        if self.restore_in_flight == Some(evaluation.id) {
            self.restore_in_flight = None;
        }
        let report = match result {
            Ok(report) if report.generation == self.generation => report,
            Ok(_) => return false,
            Err(error) => {
                self.last_error = Some(error);
                return true;
            }
        };
        self.ready_generation = report.ready.then_some(report.generation);
        if report.ready {
            self.completed_generation = report.generation;
        }
        self.metrics = Some(HtmlScrollMetrics::from_report(report));
        self.image_metrics = Some(report.images);
        self.last_error = None;
        if evaluation.kind == EvaluationKind::Restore && report.ready {
            self.pending_restore = None;
        }
        true
    }

    fn snapshot(&self) -> HtmlViewLifecycleSnapshot {
        HtmlViewLifecycleSnapshot {
            generation: self.generation,
            completed_generation: self.completed_generation,
            ready: self.ready_generation == Some(self.generation),
            evaluation_pending: self
                .evaluations
                .values()
                .any(|evaluation| evaluation.generation == self.generation),
            pending_restore: self
                .pending_restore
                .filter(|pending| pending.generation == self.generation)
                .map(|pending| pending.fraction),
            scroll: self.metrics,
            images: self.image_metrics,
            last_error: self.last_error.clone(),
        }
    }
}

struct HtmlViewLifecycleInner {
    view: webkit6::WebView,
    status_label: gtk::Label,
    state: RefCell<LifecycleState>,
}

#[derive(Clone)]
pub(crate) struct HtmlViewLifecycle {
    inner: Rc<HtmlViewLifecycleInner>,
}

impl HtmlViewLifecycle {
    pub(crate) fn new(view: &webkit6::WebView, status_label: &gtk::Label) -> Self {
        let lifecycle = Self {
            inner: Rc::new(HtmlViewLifecycleInner {
                view: view.clone(),
                status_label: status_label.clone(),
                state: RefCell::new(LifecycleState::default()),
            }),
        };
        lifecycle.install_scroll_observer();
        lifecycle.connect_load_events();
        lifecycle
    }

    pub(crate) fn load_html(&self, document: &str, base_uri: Option<&str>) -> u64 {
        let generation = self.inner.state.borrow_mut().begin_load();
        // Invalidate callback state before stopping a superseded load. Any
        // completion event emitted by `stop_loading` can then only observe the
        // new generation and will be rejected by the document token check.
        self.inner.view.stop_loading();
        let document = document_with_generation(document, generation);
        self.inner.view.load_html(&document, base_uri);
        generation
    }

    pub(crate) fn scroll_lines(&self, lines: f64) {
        self.evaluate_scroll(&format!("e.scrollBy(0, {});", (lines * 40.0).round()));
    }

    pub(crate) fn scroll_pages(&self, pages: f64) {
        self.evaluate_scroll(&format!(
            "e.scrollBy(0, Math.round(window.innerHeight * {pages}));"
        ));
    }

    pub(crate) fn scroll_to_edge(&self, bottom: bool) {
        self.evaluate_scroll(if bottom {
            "e.scrollTo(0, e.scrollHeight);"
        } else {
            "e.scrollTo(0, 0);"
        });
    }

    pub(crate) fn scroll_fraction(&self) -> Option<f64> {
        self.inner
            .state
            .borrow()
            .snapshot()
            .scroll
            .map(|metrics| metrics.fraction)
    }

    pub(crate) fn restore_fraction(&self, fraction: f64) {
        self.inner.state.borrow_mut().queue_restore(fraction);
        dispatch_ready_restore(&self.inner);
    }

    pub(crate) fn snapshot(&self) -> HtmlViewLifecycleSnapshot {
        self.inner.state.borrow().snapshot()
    }

    fn install_scroll_observer(&self) {
        let Some(manager) = self.inner.view.user_content_manager() else {
            self.inner.state.borrow_mut().last_error =
                Some("HTML view has no user content manager".to_string());
            return;
        };
        if !manager.register_script_message_handler(SCROLL_MESSAGE_HANDLER, None) {
            self.inner.state.borrow_mut().last_error =
                Some("HTML scroll observer could not be registered".to_string());
            return;
        }
        let script = webkit6::UserScript::new(
            SCROLL_OBSERVER_SCRIPT,
            webkit6::UserContentInjectedFrames::TopFrame,
            webkit6::UserScriptInjectionTime::End,
            &[],
            &[],
        );
        manager.add_script(&script);

        let inner = Rc::downgrade(&self.inner);
        manager.connect_script_message_received(Some(SCROLL_MESSAGE_HANDLER), move |_, value| {
            let Some(inner) = inner.upgrade() else {
                return;
            };
            let Ok(report) = serde_json::from_str::<ScrollReport>(&value.to_str()) else {
                return;
            };
            if inner.state.borrow_mut().record_report(report) {
                dispatch_ready_restore(&inner);
            }
        });
    }

    fn connect_load_events(&self) {
        let inner = Rc::downgrade(&self.inner);
        self.inner.view.connect_load_changed(move |_, event| {
            if event != webkit6::LoadEvent::Finished {
                return;
            }
            let Some(inner) = inner.upgrade() else {
                return;
            };
            evaluate(&inner, EvaluationKind::Probe, "");
        });
    }

    fn evaluate_scroll(&self, operation: &str) {
        evaluate(&self.inner, EvaluationKind::Scroll, operation);
    }
}

fn dispatch_ready_restore(inner: &Rc<HtmlViewLifecycleInner>) {
    let Some((evaluation, fraction)) = inner.state.borrow_mut().begin_ready_restore() else {
        return;
    };
    let operation = format!(
        "const max = Math.max(0, e.scrollHeight - e.clientHeight); e.scrollTo(0, max * {fraction});"
    );
    evaluate_started(inner, evaluation, &operation);
}

fn evaluate(inner: &Rc<HtmlViewLifecycleInner>, kind: EvaluationKind, operation: &str) {
    let evaluation = inner.state.borrow_mut().begin_evaluation(kind);
    evaluate_started(inner, evaluation, operation);
}

fn evaluate_started(inner: &Rc<HtmlViewLifecycleInner>, evaluation: Evaluation, operation: &str) {
    let script = scroll_evaluation_script(evaluation.generation, operation);
    let weak_inner: Weak<HtmlViewLifecycleInner> = Rc::downgrade(inner);
    inner.view.evaluate_javascript(
        &script,
        Some("notm-scroll"),
        Some("notm://scroll"),
        None::<&gtk::gio::Cancellable>,
        move |result| {
            let Some(inner) = weak_inner.upgrade() else {
                return;
            };
            let result = result.map_err(|error| error.to_string()).and_then(|value| {
                serde_json::from_str::<ScrollReport>(&value.to_str())
                    .map_err(|error| error.to_string())
            });
            let current = inner
                .state
                .borrow_mut()
                .complete_evaluation(evaluation, result);
            if current {
                if let Some(error) = inner.state.borrow().last_error.clone() {
                    if evaluation.kind != EvaluationKind::Probe {
                        inner
                            .status_label
                            .set_text(&format!("HTML scroll failed: {error}"));
                    }
                } else {
                    dispatch_ready_restore(&inner);
                }
            }
        },
    );
}

fn scroll_evaluation_script(generation: u64, operation: &str) -> String {
    format!(
        r#"(() => {{
  const actual = Number(document.documentElement?.dataset?.notmGeneration || "0");
  const e = document.scrollingElement || document.documentElement || document.body;
  const images = Array.from(document.images || []);
  const loaded = images.filter(image => image.complete && image.naturalWidth > 0).length;
  const failed = images.filter(image => image.complete && image.naturalWidth === 0).length;
  if (actual === {generation} && document.readyState === "complete") {{ {operation} }}
  return JSON.stringify({{
    generation: actual,
    ready: actual === {generation} && document.readyState === "complete",
    y: e?.scrollTop || 0,
    h: e?.scrollHeight || 0,
    c: e?.clientHeight || 0,
    images: {{
      total: images.length,
      loaded,
      failed,
      pending: images.length - loaded - failed
    }}
  }});
}})()"#
    )
}

fn document_with_generation(document: &str, generation: u64) -> String {
    if let Some(html_start) = document.find("<html") {
        let insert_at = html_start + "<html".len();
        let mut tagged = String::with_capacity(document.len() + 40);
        tagged.push_str(&document[..insert_at]);
        tagged.push_str(&format!(" data-notm-generation=\"{generation}\""));
        tagged.push_str(&document[insert_at..]);
        tagged
    } else {
        format!(
            "<!doctype html><html data-notm-generation=\"{generation}\"><body>{document}</body></html>"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(generation: u64, ready: bool, y: f64) -> ScrollReport {
        ScrollReport {
            generation,
            ready,
            y,
            h: 1_000.0,
            c: 200.0,
            images: HtmlImageMetrics::default(),
        }
    }

    fn report_with_images(
        generation: u64,
        ready: bool,
        y: f64,
        images: HtmlImageMetrics,
    ) -> ScrollReport {
        ScrollReport {
            images,
            ..report(generation, ready, y)
        }
    }

    #[test]
    fn stale_evaluation_cannot_replace_newer_document_state() {
        let mut state = LifecycleState::default();
        let first = state.begin_load();
        let first_probe = state.begin_evaluation(EvaluationKind::Probe);
        let second = state.begin_load();
        assert!(state.evaluations.is_empty());
        let second_probe = state.begin_evaluation(EvaluationKind::Probe);

        assert!(state.complete_evaluation(second_probe, Ok(report(second, true, 400.0))));
        assert!(!state.complete_evaluation(first_probe, Ok(report(first, true, 700.0))));

        let snapshot = state.snapshot();
        assert_eq!(snapshot.generation, second);
        assert_eq!(snapshot.completed_generation, second);
        assert!(snapshot.ready);
        assert_eq!(snapshot.scroll.expect("current metrics").y, 400.0);
    }

    #[test]
    fn mismatched_document_token_is_not_treated_as_ready() {
        let mut state = LifecycleState::default();
        let first = state.begin_load();
        let second = state.begin_load();
        let probe = state.begin_evaluation(EvaluationKind::Probe);

        assert!(!state.complete_evaluation(probe, Ok(report(first, true, 500.0))));
        let snapshot = state.snapshot();
        assert_eq!(snapshot.generation, second);
        assert_eq!(snapshot.completed_generation, 0);
        assert!(!snapshot.ready);
        assert!(snapshot.scroll.is_none());
        assert!(snapshot.images.is_none());
    }

    #[test]
    fn image_metrics_are_generation_scoped_and_reset_for_a_new_load() {
        let mut state = LifecycleState::default();
        let first = state.begin_load();
        let first_metrics = HtmlImageMetrics {
            total: 7,
            loaded: 5,
            failed: 1,
            pending: 1,
        };
        assert!(state.record_report(report_with_images(first, true, 0.0, first_metrics)));
        assert_eq!(state.snapshot().images, Some(first_metrics));

        let second = state.begin_load();
        assert!(state.snapshot().images.is_none());
        assert!(!state.record_report(report_with_images(
            first,
            true,
            0.0,
            HtmlImageMetrics {
                total: 99,
                loaded: 99,
                failed: 0,
                pending: 0,
            }
        )));
        assert!(state.snapshot().images.is_none());

        let second_metrics = HtmlImageMetrics {
            total: 4,
            loaded: 3,
            failed: 1,
            pending: 0,
        };
        assert!(state.record_report(report_with_images(second, true, 0.0, second_metrics)));
        assert_eq!(state.snapshot().images, Some(second_metrics));
    }

    #[test]
    fn legacy_reports_default_missing_image_metrics_to_zero() {
        let report: ScrollReport =
            serde_json::from_str(r#"{"generation":1,"ready":true,"y":0,"h":100,"c":50}"#)
                .expect("legacy lifecycle report");

        assert_eq!(report.images, HtmlImageMetrics::default());
    }

    #[test]
    fn restore_waits_for_readiness_and_is_generation_scoped() {
        let mut state = LifecycleState::default();
        let generation = state.begin_load();
        state.queue_restore(0.75);
        assert!(state.begin_ready_restore().is_none());

        assert!(state.record_report(report(generation, false, 0.0)));
        assert!(state.begin_ready_restore().is_none());
        assert!(state.record_report(report(generation, true, 0.0)));
        let (restore, fraction) = state.begin_ready_restore().expect("ready restore");
        assert_eq!(fraction, 0.75);
        assert!(state.complete_evaluation(restore, Ok(report(generation, true, 600.0))));
        let snapshot = state.snapshot();
        assert_eq!(snapshot.completed_generation, generation);
        assert_eq!(snapshot.pending_restore, None);
        assert_eq!(snapshot.scroll.expect("restored metrics").fraction, 0.75);

        state.queue_restore(0.25);
        state.begin_load();
        assert_eq!(state.snapshot().pending_restore, None);
    }

    #[test]
    fn incomplete_restore_completion_keeps_the_request_queued() {
        let mut state = LifecycleState::default();
        let generation = state.begin_load();
        state.queue_restore(0.5);
        assert!(state.record_report(report(generation, true, 0.0)));
        let (restore, _) = state.begin_ready_restore().expect("ready restore");

        assert!(state.complete_evaluation(restore, Ok(report(generation, false, 0.0))));
        let snapshot = state.snapshot();
        assert!(!snapshot.ready);
        assert_eq!(snapshot.pending_restore, Some(0.5));
        assert!(state.begin_ready_restore().is_none());
    }

    #[test]
    fn documents_receive_a_generation_token() {
        assert_eq!(
            document_with_generation("<!doctype html><html><body>x</body></html>", 42),
            "<!doctype html><html data-notm-generation=\"42\"><body>x</body></html>"
        );
        assert_eq!(
            document_with_generation("fragment", 7),
            "<!doctype html><html data-notm-generation=\"7\"><body>fragment</body></html>"
        );
    }
}
