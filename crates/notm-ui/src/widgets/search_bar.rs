use std::{
    cell::{Cell, RefCell, RefMut},
    rc::Rc,
    time::Duration,
};

use gtk::prelude::*;
use gtk4 as gtk;

const SEARCH_DEBOUNCE: Duration = Duration::from_millis(350);
const COMPLETION_FOCUS_LEAVE_DELAY: Duration = Duration::from_millis(150);
const MAX_FIXTURE_SEARCH_DELAY: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
struct SearchCompletionSession {
    base: String,
    cursor_position: i32,
    suggestions: Vec<String>,
    next_index: usize,
    generated_text: Option<String>,
    suppress_next_change: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct SearchWorkerRequest {
    pub(crate) query: String,
    pub(crate) generation: u64,
    pub(crate) select_first: bool,
    pub(crate) delay: Duration,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SearchHarnessPolicy {
    pub(crate) fixture_mode: bool,
    pub(crate) automation_enabled: bool,
    pub(crate) allow_live_tag_test: bool,
}

pub(crate) enum SearchInputEvent {
    Cleared,
    Reserved { query: String, generation: u64 },
    Dispatch(SearchWorkerRequest),
}

pub(crate) type SearchInputHandler = Rc<dyn Fn(SearchInputEvent)>;
pub(crate) type VisibleTagsProvider = Rc<dyn Fn() -> Vec<String>>;

pub(crate) trait SearchActivityState {
    fn search_generation(&self) -> u64;
    fn set_search_generation(&mut self, generation: u64);
    fn set_search_loading(&mut self, loading: bool);
    fn set_pending_search_query(&mut self, query: Option<String>);
    fn set_search_error(&mut self, error: Option<String>);
}

impl<T> SearchActivityState for RefMut<'_, T>
where
    T: SearchActivityState + ?Sized,
{
    fn search_generation(&self) -> u64 {
        (**self).search_generation()
    }

    fn set_search_generation(&mut self, generation: u64) {
        (**self).set_search_generation(generation);
    }

    fn set_search_loading(&mut self, loading: bool) {
        (**self).set_search_loading(loading);
    }

    fn set_pending_search_query(&mut self, query: Option<String>) {
        (**self).set_pending_search_query(query);
    }

    fn set_search_error(&mut self, error: Option<String>) {
        (**self).set_search_error(error);
    }
}

pub(crate) fn begin_search_activity<S>(state: &mut S, generation: u64, query: &str)
where
    S: SearchActivityState,
{
    state.set_search_loading(true);
    state.set_search_generation(generation);
    state.set_pending_search_query(Some(query.to_string()));
    state.set_search_error(None);
}

pub(crate) fn finish_search_activity<S>(state: &mut S, generation: u64) -> bool
where
    S: SearchActivityState,
{
    if state.search_generation() != generation {
        return false;
    }
    state.set_search_loading(false);
    state.set_pending_search_query(None);
    true
}

pub(crate) fn cancel_search_activity<S>(state: &mut S, generation: u64)
where
    S: SearchActivityState,
{
    state.set_search_loading(false);
    state.set_search_generation(generation);
    state.set_pending_search_query(None);
    state.set_search_error(None);
}

#[derive(Clone)]
pub(crate) struct SearchBarController {
    root: gtk::Box,
    entry: gtk::Entry,
    button: gtk::Button,
    suggestions: gtk::ListBox,
    generation: Rc<Cell<u64>>,
    requested_query: Rc<RefCell<String>>,
    completion: Rc<RefCell<Option<SearchCompletionSession>>>,
}

#[derive(Clone)]
struct SearchBarSignalState {
    entry: gtk::glib::WeakRef<gtk::Entry>,
    suggestions: gtk::glib::WeakRef<gtk::ListBox>,
    generation: Rc<Cell<u64>>,
    requested_query: Rc<RefCell<String>>,
    completion: Rc<RefCell<Option<SearchCompletionSession>>>,
}

impl SearchBarController {
    pub(crate) fn new(default_query: &str, trailing_action: &impl IsA<gtk::Widget>) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
        root.set_hexpand(true);
        root.set_halign(gtk::Align::Fill);

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let entry = gtk::Entry::new();
        entry.set_widget_name("notm-search-entry");
        entry.set_hexpand(true);
        entry.set_text(default_query);
        entry.set_placeholder_text(Some(
            "Notmuch query, e.g. tag:inbox and not tag:trash and not tag:spam",
        ));
        let button = gtk::Button::with_label("Search");
        button.set_widget_name("notm-search-button");
        row.append(&entry);
        row.append(&button);
        row.append(trailing_action);
        root.append(&row);

        let suggestions = gtk::ListBox::new();
        suggestions.set_widget_name("notm-search-suggestions-list");
        suggestions.set_selection_mode(gtk::SelectionMode::Single);
        suggestions.add_css_class("boxed-list");
        suggestions.set_hexpand(true);
        suggestions.set_focusable(false);
        suggestions.set_visible(false);
        root.append(&suggestions);

        let helper = gtk::Label::new(Some(
            "Syntax: tag:inbox, from:alice, subject:report, thread:<id>, *",
        ));
        helper.set_xalign(0.0);
        helper.add_css_class("dim-label");
        root.append(&helper);

        Self {
            root,
            entry,
            button,
            suggestions,
            generation: Rc::new(Cell::new(0)),
            requested_query: Rc::new(RefCell::new(default_query.to_string())),
            completion: Rc::new(RefCell::new(None)),
        }
    }

    pub(crate) fn root(&self) -> gtk::Box {
        self.root.clone()
    }

    pub(crate) fn entry(&self) -> gtk::Entry {
        self.entry.clone()
    }

    pub(crate) fn button(&self) -> gtk::Button {
        self.button.clone()
    }

    pub(crate) fn set_query(&self, query: &str) {
        self.entry.set_text(query);
    }

    pub(crate) fn focus(&self) {
        self.entry.grab_focus();
    }

    pub(crate) fn has_focus(&self) -> bool {
        widget_contains_focus(self.entry.upcast_ref())
    }

    pub(crate) fn suggestions_visible(&self) -> bool {
        self.suggestions.is_visible()
    }

    pub(crate) fn current_generation(&self) -> u64 {
        self.generation.get()
    }

    pub(crate) fn set_generation(&self, generation: u64) {
        self.generation.set(generation);
    }

    pub(crate) fn requested_query(&self) -> String {
        self.requested_query.borrow().clone()
    }

    pub(crate) fn set_requested_query(&self, query: &str) {
        query.clone_into(&mut self.requested_query.borrow_mut());
    }

    pub(crate) fn connect_debounce(&self, handler: SearchInputHandler) {
        let debounce_generation = Rc::new(Cell::new(0_u64));
        let signal_state = self.signal_state();
        self.entry.connect_changed(move |entry| {
            let query = entry.text().to_string();
            let debounce = debounce_generation.get().saturating_add(1);
            debounce_generation.set(debounce);
            if query.trim().is_empty() {
                signal_state.clear_requested_query();
                handler(SearchInputEvent::Cleared);
                return;
            }

            let generation = signal_state.reserve_generation();
            signal_state.set_requested_query(&query);
            handler(SearchInputEvent::Reserved {
                query: query.clone(),
                generation,
            });

            let signal_state = signal_state.clone();
            let handler = handler.clone();
            let debounce_generation = debounce_generation.clone();
            gtk::glib::timeout_add_local_once(SEARCH_DEBOUNCE, move || {
                if debounce != debounce_generation.get()
                    || generation != signal_state.current_generation()
                {
                    return;
                }
                handler(SearchInputEvent::Dispatch(SearchWorkerRequest {
                    query,
                    generation,
                    select_first: !signal_state.has_focus(),
                    delay: Duration::ZERO,
                }));
            });
        });
    }

    pub(crate) fn connect_autocomplete(&self, visible_tags: VisibleTagsProvider) {
        let completion_active = Rc::new(Cell::new(false));
        let focus_generation = Rc::new(Cell::new(0_u64));
        let signal_state = self.signal_state();
        let active = completion_active.clone();
        let tags = visible_tags.clone();
        self.entry.connect_changed(move |entry| {
            let text = entry.text().to_string();
            {
                let mut session_ref = signal_state.completion.borrow_mut();
                if let Some(session) = session_ref.as_mut()
                    && session.suppress_next_change
                {
                    if session.generated_text.as_deref() == Some(text.as_str()) {
                        session.suppress_next_change = false;
                        return;
                    }
                    if text.is_empty() && session.generated_text.is_some() {
                        return;
                    }
                }
            }
            if signal_state.completion_current_matches(&text) {
                return;
            }
            signal_state.reset_completion();
            if active.get() {
                signal_state.update_suggestions(&text, entry.position(), &tags());
            } else {
                signal_state.hide_suggestions();
            }
        });

        let key_controller = gtk::EventControllerKey::new();
        key_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        let signal_state = self.signal_state();
        let active = completion_active.clone();
        let tags = visible_tags.clone();
        key_controller.connect_key_pressed(move |_, key, _, _| {
            active.set(true);
            if key == gtk::gdk::Key::Tab && signal_state.apply_next_completion(&tags()) {
                return gtk::glib::Propagation::Stop;
            }
            if key == gtk::gdk::Key::Escape {
                signal_state.reset_completion();
                signal_state.hide_suggestions();
            }
            gtk::glib::Propagation::Proceed
        });
        self.entry.add_controller(key_controller);

        let focus = gtk::EventControllerFocus::new();
        let signal_state = self.signal_state();
        let active = completion_active.clone();
        let generation = focus_generation.clone();
        let tags = visible_tags;
        focus.connect_enter(move |_| {
            active.set(true);
            generation.set(generation.get().saturating_add(1));
            signal_state.update_current_suggestions(&tags());
        });
        let signal_state = self.signal_state();
        let active = completion_active;
        let generation = focus_generation;
        focus.connect_leave(move |_| {
            let leave_generation = generation.get().saturating_add(1);
            generation.set(leave_generation);
            let signal_state = signal_state.clone();
            let active = active.clone();
            let generation = generation.clone();
            gtk::glib::timeout_add_local_once(COMPLETION_FOCUS_LEAVE_DELAY, move || {
                if generation.get() == leave_generation {
                    active.set(false);
                    signal_state.hide_suggestions();
                }
            });
        });
        self.entry.add_controller(focus);

        let signal_state = self.signal_state();
        self.suggestions.connect_row_activated(move |_, row| {
            let Some(child) = row.child() else {
                return;
            };
            let Ok(label) = child.downcast::<gtk::Label>() else {
                return;
            };
            signal_state.apply_completion(&label.text());
            signal_state.reset_completion();
            signal_state.hide_suggestions();
        });
    }

    fn signal_state(&self) -> SearchBarSignalState {
        SearchBarSignalState {
            entry: self.entry.downgrade(),
            suggestions: self.suggestions.downgrade(),
            generation: self.generation.clone(),
            requested_query: self.requested_query.clone(),
            completion: self.completion.clone(),
        }
    }
}

impl SearchBarSignalState {
    fn current_generation(&self) -> u64 {
        self.generation.get()
    }

    fn reserve_generation(&self) -> u64 {
        let generation = self.generation.get().saturating_add(1);
        self.generation.set(generation);
        generation
    }

    fn set_requested_query(&self, query: &str) {
        query.clone_into(&mut self.requested_query.borrow_mut());
    }

    fn clear_requested_query(&self) {
        self.requested_query.borrow_mut().clear();
    }

    fn has_focus(&self) -> bool {
        self.entry
            .upgrade()
            .is_some_and(|entry| widget_contains_focus(entry.upcast_ref()))
    }

    fn update_current_suggestions(&self, visible_tags: &[String]) {
        let Some(entry) = self.entry.upgrade() else {
            return;
        };
        self.update_suggestions(&entry.text(), entry.position(), visible_tags);
    }

    fn update_suggestions(&self, input: &str, cursor_position: i32, visible_tags: &[String]) {
        let suggestions = matching_search_suggestions(input, cursor_position, visible_tags, 8);
        if suggestions.is_empty() {
            self.hide_suggestions();
            return;
        }
        *self.completion.borrow_mut() = Some(SearchCompletionSession {
            base: input.to_string(),
            cursor_position,
            suggestions: suggestions.clone(),
            next_index: 0,
            generated_text: None,
            suppress_next_change: false,
        });
        self.populate_suggestions(&suggestions);
        let Some(suggestions) = self.suggestions.upgrade() else {
            return;
        };
        let width = self
            .entry
            .upgrade()
            .map_or(360, |entry| entry.width().max(360));
        suggestions.set_size_request(width, -1);
        suggestions.set_visible(true);
    }

    fn hide_suggestions(&self) {
        self.populate_suggestions(&[]);
        if let Some(suggestions) = self.suggestions.upgrade() {
            suggestions.set_visible(false);
        }
    }

    fn reset_completion(&self) {
        *self.completion.borrow_mut() = None;
    }

    fn populate_suggestions(&self, suggestions: &[String]) {
        let Some(list) = self.suggestions.upgrade() else {
            return;
        };
        while let Some(child) = list.first_child() {
            list.remove(&child);
        }
        for suggestion in suggestions {
            let row = gtk::ListBoxRow::new();
            row.set_widget_name(&format!(
                "notm-search-suggestion-{}",
                suggestion_widget_token(suggestion)
            ));
            row.set_focusable(false);
            let label = gtk::Label::new(Some(suggestion));
            label.set_xalign(0.0);
            label.set_margin_start(6);
            label.set_margin_end(6);
            label.set_margin_top(3);
            label.set_margin_bottom(3);
            row.set_child(Some(&label));
            list.append(&row);
        }
    }

    fn apply_completion(&self, replacement: &str) {
        let Some(entry) = self.entry.upgrade() else {
            return;
        };
        let current = entry.text();
        let (next, cursor) = search_completion_text(&current, entry.position(), replacement);
        entry.set_text(&next);
        entry.set_position(cursor);
    }

    fn apply_next_completion(&self, visible_tags: &[String]) -> bool {
        let Some(entry) = self.entry.upgrade() else {
            return false;
        };
        let current = entry.text().to_string();
        let reuse_session = self
            .completion
            .borrow()
            .as_ref()
            .is_some_and(|session| search_session_matches_current(session, &current));
        if !reuse_session {
            let suggestions =
                matching_search_suggestions(&current, entry.position(), visible_tags, 20);
            if suggestions.is_empty() {
                self.hide_suggestions();
                return false;
            }
            *self.completion.borrow_mut() = Some(SearchCompletionSession {
                base: current.clone(),
                cursor_position: entry.position(),
                suggestions,
                next_index: 0,
                generated_text: None,
                suppress_next_change: false,
            });
        }

        let (next, cursor, index, suggestions) = {
            let mut session_ref = self.completion.borrow_mut();
            let Some(session) = session_ref.as_mut() else {
                return false;
            };
            if session.suggestions.is_empty() {
                *session_ref = None;
                return false;
            }
            if let Some(current_index) = search_generated_index(session, &current) {
                session.next_index = current_index.saturating_add(1);
            }
            let index = session.next_index % session.suggestions.len();
            let (next, cursor) = search_completion_text(
                &session.base,
                session.cursor_position,
                &session.suggestions[index],
            );
            session.generated_text = Some(next.clone());
            session.suppress_next_change = true;
            session.next_index = index + 1;
            (next, cursor, index, session.suggestions.clone())
        };

        entry.set_text(&next);
        entry.set_position(cursor);
        self.populate_suggestions(&suggestions);
        if let Some(list) = self.suggestions.upgrade() {
            list.set_visible(true);
            if let Some(row) = list.row_at_index(index as i32) {
                list.select_row(Some(&row));
            }
        }
        true
    }

    fn completion_current_matches(&self, text: &str) -> bool {
        self.completion
            .borrow()
            .as_ref()
            .is_some_and(|session| search_session_matches_current(session, text))
    }
}

pub(crate) fn fixture_search_worker_delay(
    policy: SearchHarnessPolicy,
    args: &serde_json::Value,
) -> anyhow::Result<Duration> {
    let Some(value) = args.get("test_delay_ms") else {
        return Ok(Duration::ZERO);
    };
    anyhow::ensure!(
        policy.automation_enabled && (policy.fixture_mode || policy.allow_live_tag_test),
        "test_delay_ms requires fixture mode or automation.allow_live_tag_test=true"
    );
    let milliseconds = value
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("test_delay_ms must be a non-negative whole number"))?;
    let delay = Duration::from_millis(milliseconds);
    anyhow::ensure!(
        delay <= MAX_FIXTURE_SEARCH_DELAY,
        "test_delay_ms must not exceed {}",
        MAX_FIXTURE_SEARCH_DELAY.as_millis()
    );
    Ok(delay)
}

fn matching_search_suggestions(
    input: &str,
    cursor_position: i32,
    visible_tags: &[String],
    limit: usize,
) -> Vec<String> {
    let cursor = char_index_to_byte(input, cursor_position.max(0) as usize);
    let (start, end) = search_token_bounds(input, cursor);
    let token = input[start..end].trim();
    if token.is_empty() {
        return Vec::new();
    }
    let token_lower = token.to_lowercase();
    let mut candidates = Vec::new();
    if let Some(tag_prefix) = token_lower.strip_prefix("tag:") {
        let raw_prefix = tag_prefix.trim_matches('"').trim_matches('\'');
        for tag in visible_tags {
            let tag_lower = tag.to_lowercase();
            if raw_prefix.is_empty()
                || tag_lower.starts_with(raw_prefix)
                || tag_lower.contains(raw_prefix)
            {
                candidates.push(format!("tag:{}", quote_notmuch_value(tag)));
            }
        }
    } else {
        candidates.extend(
            [
                "tag:",
                "from:",
                "to:",
                "cc:",
                "subject:",
                "thread:",
                "id:",
                "date:",
                "folder:",
                "path:",
                "property:",
                "and",
                "or",
                "not",
                "*",
            ]
            .into_iter()
            .filter(|candidate| candidate.starts_with(&token_lower))
            .map(str::to_string),
        );
        for tag in visible_tags {
            if tag.to_lowercase().starts_with(&token_lower) {
                candidates.push(format!("tag:{}", quote_notmuch_value(tag)));
            }
        }
    }
    candidates.sort();
    candidates.dedup();
    candidates.truncate(limit);
    candidates
}

fn search_session_matches_current(session: &SearchCompletionSession, current: &str) -> bool {
    session.base == current
        || session.generated_text.as_deref() == Some(current)
        || search_generated_index(session, current).is_some()
}

fn search_generated_index(session: &SearchCompletionSession, current: &str) -> Option<usize> {
    session.suggestions.iter().position(|suggestion| {
        search_completion_text(&session.base, session.cursor_position, suggestion).0 == current
    })
}

fn search_completion_text(current: &str, cursor_position: i32, replacement: &str) -> (String, i32) {
    let cursor = char_index_to_byte(current, cursor_position.max(0) as usize);
    let (start, end) = search_token_bounds(current, cursor);
    let replacement = if replacement.ends_with(' ') || replacement.ends_with(':') {
        replacement.to_string()
    } else {
        format!("{replacement} ")
    };
    let next = format!("{}{}{}", &current[..start], replacement, &current[end..]);
    let next_cursor = start + replacement.len();
    (next.clone(), byte_index_to_char(&next, next_cursor))
}

fn search_token_bounds(input: &str, cursor: usize) -> (usize, usize) {
    let cursor = cursor.min(input.len());
    let start = input[..cursor]
        .char_indices()
        .rev()
        .find(|(_, ch)| search_token_separator(*ch))
        .map(|(index, ch)| index + ch.len_utf8())
        .unwrap_or(0);
    let end = input[cursor..]
        .char_indices()
        .find(|(_, ch)| search_token_separator(*ch))
        .map(|(index, _)| cursor + index)
        .unwrap_or(input.len());
    (start, end)
}

fn search_token_separator(ch: char) -> bool {
    ch.is_whitespace() || matches!(ch, '(' | ')')
}

fn char_index_to_byte(input: &str, char_index: usize) -> usize {
    input
        .char_indices()
        .nth(char_index)
        .map(|(index, _)| index)
        .unwrap_or(input.len())
}

fn byte_index_to_char(input: &str, byte_index: usize) -> i32 {
    input[..byte_index.min(input.len())].chars().count() as i32
}

pub(crate) fn quote_notmuch_value(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | '@'))
    {
        value.to_string()
    } else {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

fn suggestion_widget_token(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn widget_contains_focus(widget: &gtk::Widget) -> bool {
    widget.has_focus() || widget.focus_child().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct ActivityState {
        loading: bool,
        generation: u64,
        pending_query: Option<String>,
        error: Option<String>,
    }

    impl SearchActivityState for ActivityState {
        fn search_generation(&self) -> u64 {
            self.generation
        }

        fn set_search_generation(&mut self, generation: u64) {
            self.generation = generation;
        }

        fn set_search_loading(&mut self, loading: bool) {
            self.loading = loading;
        }

        fn set_pending_search_query(&mut self, query: Option<String>) {
            self.pending_query = query;
        }

        fn set_search_error(&mut self, error: Option<String>) {
            self.error = error;
        }
    }

    #[test]
    fn stale_search_completion_cannot_finish_the_current_generation() {
        let mut state = ActivityState::default();
        begin_search_activity(&mut state, 4, "tag:inbox");
        begin_search_activity(&mut state, 5, "tag:unread");

        assert!(!finish_search_activity(&mut state, 4));
        assert!(state.loading);
        assert_eq!(state.generation, 5);
        assert_eq!(state.pending_query.as_deref(), Some("tag:unread"));

        assert!(finish_search_activity(&mut state, 5));
        assert!(!state.loading);
        assert_eq!(state.pending_query, None);

        begin_search_activity(&mut state, 6, "tag:flagged");
        cancel_search_activity(&mut state, 7);
        assert!(!finish_search_activity(&mut state, 6));
        assert!(!state.loading);
        assert_eq!(state.generation, 7);
        assert_eq!(state.pending_query, None);
    }

    #[test]
    fn completion_replaces_the_unicode_token_at_the_character_cursor() {
        let input = "from:josé tag:in";
        let cursor = input.chars().count() as i32;
        assert_eq!(
            search_completion_text(input, cursor, "tag:inbox"),
            ("from:josé tag:inbox ".to_string(), cursor + 4)
        );
    }

    #[test]
    fn tag_completion_quotes_visible_values_without_normalizing_them() {
        assert_eq!(
            matching_search_suggestions(
                "tag:pro",
                7,
                &["project alpha".to_string(), "projects".to_string()],
                8,
            ),
            ["tag:\"project alpha\"", "tag:projects"]
        );
    }

    #[test]
    fn delayed_search_work_is_scoped_to_explicit_test_harnesses() {
        let fixture = SearchHarnessPolicy {
            fixture_mode: true,
            automation_enabled: true,
            allow_live_tag_test: false,
        };
        assert_eq!(
            fixture_search_worker_delay(fixture, &serde_json::json!({"test_delay_ms": 250}))
                .expect("fixture delay"),
            Duration::from_millis(250)
        );
        let normal = SearchHarnessPolicy {
            fixture_mode: false,
            automation_enabled: false,
            allow_live_tag_test: false,
        };
        assert_eq!(
            fixture_search_worker_delay(normal, &serde_json::json!({})).expect("no delay"),
            Duration::ZERO
        );
        assert!(
            fixture_search_worker_delay(normal, &serde_json::json!({"test_delay_ms": 1}))
                .unwrap_err()
                .to_string()
                .contains("requires fixture mode")
        );
        let isolated_live = SearchHarnessPolicy {
            fixture_mode: false,
            automation_enabled: true,
            allow_live_tag_test: true,
        };
        assert_eq!(
            fixture_search_worker_delay(isolated_live, &serde_json::json!({"test_delay_ms": 250}))
                .expect("explicitly gated live delay"),
            Duration::from_millis(250)
        );
        assert!(
            fixture_search_worker_delay(fixture, &serde_json::json!({"test_delay_ms": 5001}))
                .unwrap_err()
                .to_string()
                .contains("must not exceed")
        );
    }
}
