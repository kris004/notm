//! The production refresh receiver. Requests never open/present a window and
//! never enter the sync, send, or tag action paths.

use super::*;
use crate::remote_refresh::{INTERFACE, INTERFACE_XML, OBJECT_PATH};

const MAX_PENDING: usize = 64;
const POLL_INTERVAL: Duration = Duration::from_millis(25);

pub(super) fn register_on_startup(
    app: &gtk::Application,
    options: &LaunchOptions,
    main_window: &Rc<RefCell<Option<MainWindowHandle>>>,
) {
    let options = options.clone();
    let main_window = main_window.clone();
    let registration = Rc::new(RefCell::new(None));
    let startup_registration = registration.clone();
    app.connect_startup(move |app| {
        let Some(connection) = app.dbus_connection() else {
            tracing::warn!("search refresh unavailable: no session bus");
            return;
        };
        let interface = gtk::gio::DBusNodeInfo::for_xml(INTERFACE_XML)
            .expect("valid refresh interface")
            .lookup_interface(INTERFACE)
            .expect("refresh interface exists");
        let options = options.clone();
        let main_window = main_window.clone();
        let pending = Rc::new(Cell::new(0usize));
        match connection
            .register_object(OBJECT_PATH, &interface)
            .method_call(move |_, _, _, _, method, parameters, invocation| {
                if method != "Refresh" {
                    invocation.return_dbus_error(
                        "org.freedesktop.DBus.Error.UnknownMethod",
                        "unsupported refresh method",
                    );
                    return;
                }
                let Some((timeout_ms,)) = parameters.get::<(u32,)>() else {
                    invocation.return_dbus_error(
                        "org.freedesktop.DBus.Error.InvalidArgs",
                        "expected timeout_ms",
                    );
                    return;
                };
                if !(1..=300_000).contains(&timeout_ms) {
                    invocation.return_dbus_error(
                        "org.freedesktop.DBus.Error.InvalidArgs",
                        "timeout_ms must be between 1 and 300000",
                    );
                    return;
                }
                let Some(handle) = main_window.borrow().as_ref().cloned() else {
                    return_error(invocation, "NotReady", "no main search window is open");
                    return;
                };
                if pending.get() >= MAX_PENDING {
                    return_error(invocation, "Busy", "too many pending refresh requests");
                    return;
                }
                pending.set(pending.get() + 1);
                wait_for_refresh(
                    options.clone(),
                    handle,
                    invocation,
                    Duration::from_millis(u64::from(timeout_ms)),
                    pending.clone(),
                );
            })
            .build()
        {
            Ok(id) => *startup_registration.borrow_mut() = Some((connection, id)),
            Err(error) => tracing::error!(%error, "could not export production search refresh"),
        }
    });
    app.connect_shutdown(move |_| {
        if let Some((connection, id)) = registration.borrow_mut().take() {
            let _ = connection.unregister_object(id);
        }
    });
}

fn return_error(invocation: gtk::gio::DBusMethodInvocation, kind: &str, message: &str) {
    invocation.return_dbus_error(&format!("{INTERFACE}.{kind}"), message);
}

fn wait_for_refresh(
    options: LaunchOptions,
    handle: MainWindowHandle,
    invocation: gtk::gio::DBusMethodInvocation,
    timeout: Duration,
    pending: Rc<Cell<usize>>,
) {
    let mut invocation = Some(invocation);
    let deadline = Instant::now() + timeout;
    // A search already in progress can predate the external database update.
    // Require a full search started AFTER this request. Concurrent requests
    // waiting for the same generation naturally coalesce.
    let mut minimum_generation = handle.state.borrow().search_generation.saturating_add(1);
    gtk::glib::timeout_add_local(POLL_INTERVAL, move || {
        let result = poll_refresh(&options, &handle, &mut minimum_generation, deadline);
        let Some(result) = result else {
            return gtk::glib::ControlFlow::Continue;
        };
        pending.set(pending.get() - 1);
        let invocation = invocation.take().expect("one refresh reply");
        match result {
            Ok(generation) => invocation.return_value(Some(&(generation,).to_variant())),
            Err((kind, message)) => return_error(invocation, kind, &message),
        }
        gtk::glib::ControlFlow::Break
    });
}

type RefreshResult = Result<u64, (&'static str, String)>;

fn poll_refresh(
    options: &LaunchOptions,
    handle: &MainWindowHandle,
    minimum_generation: &mut u64,
    deadline: Instant,
) -> Option<RefreshResult> {
    let widgets = &handle.widgets;
    let state = &handle.state;
    if !handle.window.is_visible()
        || widgets.close_when_idle.get()
        || widgets.close_flush_in_progress.get()
    {
        return Some(Err((
            "Closed",
            "main search window is closing or closed".into(),
        )));
    }
    if Instant::now() >= deadline {
        return Some(Err((
            "Timeout",
            "refresh deadline expired; a scheduled search may still complete".into(),
        )));
    }
    if writers_or_preparation_busy(widgets, state) {
        // In particular, don't acknowledge an earlier refresh while a writer
        // can still invalidate it. Do not cancel a mutation or a draft load.
        *minimum_generation = state.borrow().search_generation.saturating_add(1);
        return None;
    }
    if state.borrow().search_loading {
        return None;
    }
    if let Some(outcome) = full_search_outcome_at_or_after(&state.borrow(), *minimum_generation) {
        return Some(
            outcome
                .map(|()| state.borrow().full_search_outcome_generation)
                .map_err(|error| ("SearchFailed", error)),
        );
    }

    // Use the completed active query, not unfinished text in the search entry.
    // Retain an exact selected thread where the first refreshed page contains
    // it. Existing reconciliation preserves composer fields and active drafts;
    // it deliberately clears visual/multi-selection and stale message caches.
    let query = state.borrow().current_query.clone();
    let selected_thread_id = selected_thread_id_for_tag_refresh(&state.borrow(), false);
    let generation = schedule_search(options, widgets, state, &query, false, Duration::ZERO);
    widgets
        .refresh_selected_thread_id
        .replace(selected_thread_id.map(|thread_id| (generation, thread_id)));
    widgets.external_refresh_generation.set(Some(generation));
    None
}

fn writers_or_preparation_busy(widgets: &Widgets, state: &SharedState) -> bool {
    let state = state.borrow();
    state.tag_in_progress
        || state.send_in_progress
        || state.sync_in_progress
        || state.pending_open_message_id.is_some()
        || widgets.draft_save_active.get().is_some()
        || widgets.composer.has_pending_confirmation()
        || widgets.composer_attachment_cache_active.get().is_some()
        || widgets
            .named_draft_io_coordinator
            .borrow()
            .migration_in_progress()
        || widgets
            .thread_load_coordinator
            .borrow()
            .active_generation()
            .is_some()
        || widgets
            .composer_preparation_coordinator
            .borrow()
            .active_generation()
            .is_some()
        || widgets
            .draft_recovery_coordinator
            .borrow()
            .active_generation()
            .is_some()
}
