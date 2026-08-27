use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
    sync::{Arc, mpsc},
    thread,
    time::Duration,
};

use gtk4::glib;

use crate::{model::ComposeFields, widgets::composer};

pub(crate) const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(350);
const WORKER_POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RecoveryAction {
    Persist(Box<ComposeFields>),
    Clear,
}

#[derive(Debug, Clone)]
pub(crate) struct DraftAutosaveEvent {
    pub(crate) generation: u64,
    pub(crate) result: Result<(), String>,
    pub(crate) is_latest: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct DraftAutosaveSnapshot {
    pub(crate) busy: bool,
    pub(crate) pending_generation: Option<u64>,
    pub(crate) completed_generation: Option<u64>,
    pub(crate) write_count: u64,
}

#[derive(Debug, Clone)]
struct DraftWriteRequest {
    request_id: u64,
    generation: u64,
    action: RecoveryAction,
    delay: Duration,
}

type Writer = Arc<dyn Fn(&RecoveryAction) -> anyhow::Result<()> + Send + Sync>;
type EventHandler = Rc<dyn Fn(DraftAutosaveEvent)>;
type FlushCallback = Box<dyn FnOnce(Result<(), String>)>;

struct FlushWaiter {
    request_id: u64,
    callback: FlushCallback,
}

#[derive(Clone)]
pub(crate) struct DraftAutosaveController(Rc<RefCell<DraftAutosaveState>>);

struct DraftAutosaveState {
    writer: Writer,
    handler: Option<EventHandler>,
    pending: Option<DraftWriteRequest>,
    pending_ready: bool,
    urgent: VecDeque<DraftWriteRequest>,
    active: Option<DraftWriteRequest>,
    debounce_source: Option<glib::SourceId>,
    next_request_id: u64,
    latest_request_id: u64,
    last_completed: Option<(u64, u64, Result<(), String>)>,
    waiters: Vec<FlushWaiter>,
    test_delay: Rc<Cell<Duration>>,
    fail_next_for_test: Rc<Cell<bool>>,
    write_count: u64,
}

impl DraftAutosaveController {
    pub(crate) fn new(
        recovery_path: std::path::PathBuf,
        legacy_recovery_path: Option<std::path::PathBuf>,
    ) -> Self {
        Self::with_writer(Arc::new(move |action| match action {
            RecoveryAction::Persist(fields) => composer::persist_recovery_draft(
                &recovery_path,
                legacy_recovery_path.as_deref(),
                fields,
            ),
            RecoveryAction::Clear => composer::clear_recovery_draft_files(
                &recovery_path,
                legacy_recovery_path.as_deref(),
            ),
        }))
    }

    fn with_writer(writer: Writer) -> Self {
        Self(Rc::new(RefCell::new(DraftAutosaveState {
            writer,
            handler: None,
            pending: None,
            pending_ready: false,
            urgent: VecDeque::new(),
            active: None,
            debounce_source: None,
            next_request_id: 1,
            latest_request_id: 0,
            last_completed: None,
            waiters: Vec::new(),
            test_delay: Rc::new(Cell::new(Duration::ZERO)),
            fail_next_for_test: Rc::new(Cell::new(false)),
            write_count: 0,
        })))
    }

    pub(crate) fn connect_events(&self, handler: EventHandler) {
        self.0.borrow_mut().handler = Some(handler);
    }

    pub(crate) fn schedule(&self, generation: u64, action: RecoveryAction, debounce: Duration) {
        self.cancel_debounce();
        self.queue_debounced(generation, action);
        let weak = Rc::downgrade(&self.0);
        let source = glib::timeout_add_local_once(debounce, move || {
            let Some(state) = weak.upgrade() else {
                return;
            };
            {
                let mut state = state.borrow_mut();
                state.debounce_source = None;
                state.pending_ready = true;
            }
            start_pending_write(&state);
        });
        self.0.borrow_mut().debounce_source = Some(source);
    }

    pub(crate) fn flush<F>(&self, generation: u64, action: RecoveryAction, callback: F)
    where
        F: FnOnce(Result<(), String>) + 'static,
    {
        self.cancel_debounce();
        let request_id = self.queue_flush(generation, action);
        self.0.borrow_mut().waiters.push(FlushWaiter {
            request_id,
            callback: Box::new(callback),
        });
        start_pending_write(&self.0);
    }

    pub(crate) fn set_test_delay(&self, delay: Duration) {
        self.0.borrow().test_delay.set(delay);
    }

    pub(crate) fn fail_next_for_test(&self) {
        self.0.borrow().fail_next_for_test.set(true);
    }

    pub(crate) fn snapshot(&self) -> DraftAutosaveSnapshot {
        let state = self.0.borrow();
        DraftAutosaveSnapshot {
            busy: state.active.is_some() || state.pending.is_some() || !state.urgent.is_empty(),
            pending_generation: state
                .urgent
                .front()
                .map(|request| request.generation)
                .or_else(|| state.pending.as_ref().map(|request| request.generation))
                .or_else(|| state.active.as_ref().map(|request| request.generation)),
            completed_generation: state
                .last_completed
                .as_ref()
                .map(|(_, generation, _)| *generation),
            write_count: state.write_count,
        }
    }

    fn queue_debounced(&self, generation: u64, action: RecoveryAction) {
        let mut state = self.0.borrow_mut();
        let request_id = next_request_id(&mut state);
        let delay = state.test_delay.get();
        state.pending = Some(DraftWriteRequest {
            request_id,
            generation,
            action,
            delay,
        });
        state.pending_ready = false;
    }

    fn queue_flush(&self, generation: u64, action: RecoveryAction) -> u64 {
        let mut state = self.0.borrow_mut();
        if let Some(request_id) = state
            .active
            .iter()
            .chain(state.urgent.iter())
            .find(|request| request.generation == generation && request.action == action)
            .map(|request| request.request_id)
        {
            return request_id;
        }

        if let Some(pending) = state.pending.take() {
            state.pending_ready = false;
            if pending.generation == generation && pending.action == action {
                let request_id = pending.request_id;
                state.urgent.push_back(pending);
                return request_id;
            }
            if pending.generation > generation {
                state.pending = Some(pending);
                // `flush` cancelled the pending request's debounce source. Keep
                // the newer edit runnable after the urgent boundary request
                // instead of leaving it stranded with no source to mark it
                // ready.
                state.pending_ready = true;
            }
        }

        let request_id = next_request_id(&mut state);
        let delay = state.test_delay.get();
        state.urgent.push_back(DraftWriteRequest {
            request_id,
            generation,
            action,
            delay,
        });
        request_id
    }

    fn cancel_debounce(&self) {
        if let Some(source) = self.0.borrow_mut().debounce_source.take() {
            source.remove();
        }
    }
}

fn next_request_id(state: &mut DraftAutosaveState) -> u64 {
    let request_id = state.next_request_id;
    state.next_request_id = state.next_request_id.saturating_add(1);
    state.latest_request_id = request_id;
    request_id
}

fn start_pending_write(state: &Rc<RefCell<DraftAutosaveState>>) {
    let request = {
        let mut state = state.borrow_mut();
        if state.active.is_some() {
            return;
        }
        let request = if let Some(request) = state.urgent.pop_front() {
            request
        } else {
            if !state.pending_ready {
                return;
            }
            let Some(request) = state.pending.take() else {
                state.pending_ready = false;
                return;
            };
            state.pending_ready = false;
            request
        };
        state.active = Some(request.clone());
        state.write_count = state.write_count.saturating_add(1);
        request
    };

    let (tx, rx) = mpsc::channel();
    let (writer, fail_for_test) = {
        let state = state.borrow();
        (
            state.writer.clone(),
            state.fail_next_for_test.replace(false),
        )
    };
    let worker_request = request.clone();
    let spawn_result = thread::Builder::new()
        .name("notm-draft-autosave".to_string())
        .spawn(move || {
            if !worker_request.delay.is_zero() {
                thread::sleep(worker_request.delay);
            }
            let result = if fail_for_test {
                Err("injected draft write failure".to_string())
            } else {
                writer(&worker_request.action).map_err(|error| error.to_string())
            };
            let _ = tx.send(result);
        });

    if let Err(error) = spawn_result {
        finish_write(
            state,
            request.request_id,
            Err(format!("starting draft autosave worker: {error}")),
        );
        return;
    }

    let weak = Rc::downgrade(state);
    glib::timeout_add_local(WORKER_POLL_INTERVAL, move || match rx.try_recv() {
        Ok(result) => {
            if let Some(state) = weak.upgrade() {
                finish_write(&state, request.request_id, result);
            }
            glib::ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            if let Some(state) = weak.upgrade() {
                finish_write(
                    &state,
                    request.request_id,
                    Err("draft autosave worker disconnected".to_string()),
                );
            }
            glib::ControlFlow::Break
        }
    });
}

fn finish_write(
    state: &Rc<RefCell<DraftAutosaveState>>,
    request_id: u64,
    result: Result<(), String>,
) {
    let (handler, event, waiters) = {
        let mut state = state.borrow_mut();
        let Some(active) = state.active.take() else {
            return;
        };
        if active.request_id != request_id {
            state.active = Some(active);
            return;
        }
        let generation = active.generation;
        state.last_completed = Some((request_id, generation, result.clone()));
        let event = DraftAutosaveEvent {
            generation,
            result: result.clone(),
            is_latest: request_id >= state.latest_request_id,
        };
        let mut ready = Vec::new();
        let mut waiting = Vec::new();
        for waiter in state.waiters.drain(..) {
            if waiter.request_id == request_id {
                ready.push(waiter);
            } else {
                waiting.push(waiter);
            }
        }
        state.waiters = waiting;
        (state.handler.clone(), event, ready)
    };

    if let Some(handler) = handler {
        handler(event);
    }
    for waiter in waiters {
        (waiter.callback)(result.clone());
    }
    start_pending_write(state);
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Mutex},
        time::{Duration, Instant},
    };

    use gtk4::glib;

    use super::*;

    fn drive_until(condition: impl Fn() -> bool) {
        let context = glib::MainContext::default();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !condition() {
            assert!(Instant::now() < deadline, "autosave test timed out");
            context.iteration(true);
        }
    }

    fn fields(body: &str) -> ComposeFields {
        ComposeFields {
            body: body.to_string(),
            ..ComposeFields::default()
        }
    }

    fn persist(body: &str) -> RecoveryAction {
        RecoveryAction::Persist(Box::new(fields(body)))
    }

    fn high_rate_edits_debounce_to_the_latest_atomic_request() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let writes_for_worker = writes.clone();
        let controller = DraftAutosaveController::with_writer(Arc::new(move |action| {
            writes_for_worker.lock().unwrap().push(action.clone());
            Ok(())
        }));
        for generation in 1..=100 {
            controller.schedule(
                generation,
                persist(&format!("edit {generation}")),
                Duration::from_millis(5),
            );
        }

        drive_until(|| !controller.snapshot().busy);

        assert_eq!(*writes.lock().unwrap(), vec![persist("edit 100")]);
    }

    fn a_flush_is_not_replaced_by_a_newer_debounced_edit() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let writes_for_worker = writes.clone();
        let controller = DraftAutosaveController::with_writer(Arc::new(move |action| {
            thread::sleep(Duration::from_millis(20));
            writes_for_worker.lock().unwrap().push(action.clone());
            Ok(())
        }));
        controller.schedule(1, persist("old"), Duration::ZERO);
        drive_until(|| controller.0.borrow().active.is_some());
        let completion = Rc::new(RefCell::new(None));
        let completion_for_callback = completion.clone();
        controller.flush(2, persist("boundary"), move |result| {
            *completion_for_callback.borrow_mut() = Some(result);
        });
        controller.schedule(3, persist("new"), Duration::ZERO);
        drive_until(|| completion.borrow().is_some() && !controller.snapshot().busy);

        assert_eq!(
            *writes.lock().unwrap(),
            vec![persist("old"), persist("boundary"), persist("new")]
        );
        assert_eq!(completion.borrow().as_ref(), Some(&Ok(())));
    }

    fn flush_waits_for_the_exact_request_and_reports_failure() {
        let controller = DraftAutosaveController::with_writer(Arc::new(move |action| {
            if matches!(action, RecoveryAction::Persist(fields) if fields.body == "fail") {
                anyhow::bail!("injected write failure");
            }
            Ok(())
        }));
        let completion = Rc::new(RefCell::new(None));
        let completion_for_callback = completion.clone();
        controller.flush(7, persist("fail"), move |result| {
            *completion_for_callback.borrow_mut() = Some(result);
        });

        drive_until(|| completion.borrow().is_some());

        assert_eq!(
            completion.borrow().as_ref().unwrap(),
            &Err("injected write failure".to_string())
        );
    }

    fn older_boundary_flush_does_not_strand_a_newer_pending_edit() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let writes_for_worker = writes.clone();
        let controller = DraftAutosaveController::with_writer(Arc::new(move |action| {
            writes_for_worker.lock().unwrap().push(action.clone());
            Ok(())
        }));
        controller.schedule(3, persist("newer edit"), Duration::from_secs(30));
        let completion = Rc::new(RefCell::new(None));
        let completion_for_callback = completion.clone();
        controller.flush(2, persist("older boundary"), move |result| {
            *completion_for_callback.borrow_mut() = Some(result);
        });

        drive_until(|| completion.borrow().is_some() && !controller.snapshot().busy);

        assert_eq!(
            *writes.lock().unwrap(),
            vec![persist("older boundary"), persist("newer edit")]
        );
    }

    #[test]
    fn debounced_serial_autosave_orders_and_flushes_requests() {
        high_rate_edits_debounce_to_the_latest_atomic_request();
        a_flush_is_not_replaced_by_a_newer_debounced_edit();
        flush_waits_for_the_exact_request_and_reports_failure();
        older_boundary_flush_does_not_strand_a_newer_pending_edit();
    }
}
