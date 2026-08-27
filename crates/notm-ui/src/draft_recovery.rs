use std::{
    fs::File,
    io::{ErrorKind, Read},
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::Context;

use crate::{model::ComposeFields, widgets::composer::fields_has_content};

pub(crate) const MAX_RECOVERY_BYTES: usize = 8 * 1024 * 1024;
const MAX_FIXTURE_TEST_DELAY: Duration = Duration::from_secs(5);
const MAX_FIXTURE_TEST_GATE_WAIT: Duration = Duration::from_secs(30);
const FIXTURE_TEST_GATE_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DraftRecoverySource {
    Current(PathBuf),
    Legacy(PathBuf),
}

impl DraftRecoverySource {
    pub(crate) fn path(&self) -> &Path {
        match self {
            Self::Current(path) | Self::Legacy(path) => path,
        }
    }

    pub(crate) const fn is_legacy(&self) -> bool {
        matches!(self, Self::Legacy(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DraftRecoveryOutcome {
    NotFound,
    Empty {
        source: DraftRecoverySource,
    },
    Loaded {
        source: DraftRecoverySource,
        fields: Box<ComposeFields>,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct DraftRecoveryRequest {
    pub(crate) generation: u64,
    pub(crate) current_path: PathBuf,
    pub(crate) legacy_path: Option<PathBuf>,
    fixture_test_delay: Duration,
    fixture_test_gate: Option<PathBuf>,
}

impl DraftRecoveryRequest {
    pub(crate) fn new(
        generation: u64,
        current_path: PathBuf,
        legacy_path: Option<PathBuf>,
    ) -> Self {
        Self {
            generation,
            current_path,
            legacy_path,
            fixture_test_delay: Duration::ZERO,
            fixture_test_gate: None,
        }
    }

    pub(crate) fn with_fixture_test_delay(mut self, delay: Duration) -> Self {
        self.fixture_test_delay = delay.min(MAX_FIXTURE_TEST_DELAY);
        self
    }

    pub(crate) fn with_fixture_test_gate(mut self, gate: Option<PathBuf>) -> Self {
        self.fixture_test_gate = gate;
        self
    }

    #[cfg(test)]
    fn fixture_test_delay(&self) -> Duration {
        self.fixture_test_delay
    }
}

#[derive(Debug)]
pub(crate) struct DraftRecoveryResponse {
    pub(crate) generation: u64,
    pub(crate) result: anyhow::Result<DraftRecoveryOutcome>,
}

#[derive(Debug, Default)]
pub(crate) struct DraftRecoveryCoordinator {
    generation: u64,
    active: Option<u64>,
}

impl DraftRecoveryCoordinator {
    pub(crate) fn begin(&mut self) -> u64 {
        self.generation = self.generation.saturating_add(1);
        self.active = Some(self.generation);
        self.generation
    }

    pub(crate) fn cancel(&mut self) {
        self.generation = self.generation.saturating_add(1);
        self.active = None;
    }

    pub(crate) fn accepts(&self, generation: u64) -> bool {
        self.active == Some(generation)
    }

    pub(crate) fn active_generation(&self) -> Option<u64> {
        self.active
    }

    pub(crate) fn finish(&mut self, generation: u64) -> bool {
        if !self.accepts(generation) {
            return false;
        }
        self.active = None;
        true
    }
}

pub(crate) fn spawn(request: DraftRecoveryRequest) -> mpsc::Receiver<DraftRecoveryResponse> {
    spawn_with(request, load_recovery)
}

fn spawn_with<F>(request: DraftRecoveryRequest, loader: F) -> mpsc::Receiver<DraftRecoveryResponse>
where
    F: FnOnce(&DraftRecoveryRequest) -> anyhow::Result<DraftRecoveryOutcome> + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    let generation = request.generation;
    let worker_tx = tx.clone();
    if let Err(error) = thread::Builder::new()
        .name("notm-draft-recovery".to_string())
        .spawn(move || {
            let result = loader(&request);
            let _ = worker_tx.send(DraftRecoveryResponse { generation, result });
        })
    {
        let _ = tx.send(DraftRecoveryResponse {
            generation,
            result: Err(anyhow::anyhow!(
                "could not spawn draft recovery loader: {error}"
            )),
        });
    }
    rx
}

fn load_recovery(request: &DraftRecoveryRequest) -> anyhow::Result<DraftRecoveryOutcome> {
    wait_for_fixture_test_gate(request.fixture_test_gate.as_deref())?;
    if !request.fixture_test_delay.is_zero() {
        thread::sleep(request.fixture_test_delay);
    }
    load_recovery_with_limit(request, MAX_RECOVERY_BYTES)
}

fn wait_for_fixture_test_gate(gate: Option<&Path>) -> anyhow::Result<()> {
    let Some(gate) = gate else {
        return Ok(());
    };
    let deadline = Instant::now() + MAX_FIXTURE_TEST_GATE_WAIT;
    loop {
        if gate
            .try_exists()
            .with_context(|| format!("checking fixture recovery gate {}", gate.display()))?
        {
            return Ok(());
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "fixture recovery gate {} was not released within {MAX_FIXTURE_TEST_GATE_WAIT:?}",
            gate.display()
        );
        thread::sleep(FIXTURE_TEST_GATE_POLL_INTERVAL);
    }
}

fn load_recovery_with_limit(
    request: &DraftRecoveryRequest,
    max_bytes: usize,
) -> anyhow::Result<DraftRecoveryOutcome> {
    if let Some(bytes) = read_bounded_if_present(&request.current_path, max_bytes)? {
        return parse_recovery(
            DraftRecoverySource::Current(request.current_path.clone()),
            &bytes,
        );
    }
    if let Some(legacy_path) = request.legacy_path.as_ref()
        && let Some(bytes) = read_bounded_if_present(legacy_path, max_bytes)?
    {
        return parse_recovery(DraftRecoverySource::Legacy(legacy_path.clone()), &bytes);
    }
    Ok(DraftRecoveryOutcome::NotFound)
}

fn read_bounded_if_present(path: &Path, max_bytes: usize) -> anyhow::Result<Option<Vec<u8>>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("opening recovery draft {}", path.display()));
        }
    };
    let max_read = u64::try_from(max_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut bytes = Vec::new();
    file.take(max_read)
        .read_to_end(&mut bytes)
        .with_context(|| format!("reading recovery draft {}", path.display()))?;
    anyhow::ensure!(
        bytes.len() <= max_bytes,
        "recovery draft {} exceeds the {max_bytes}-byte limit",
        path.display()
    );
    Ok(Some(bytes))
}

fn parse_recovery(
    source: DraftRecoverySource,
    bytes: &[u8],
) -> anyhow::Result<DraftRecoveryOutcome> {
    let fields = serde_json::from_slice::<ComposeFields>(bytes)
        .with_context(|| format!("parsing recovery draft {}", source.path().display()))?;
    if fields_has_content(&fields) {
        Ok(DraftRecoveryOutcome::Loaded {
            source,
            fields: Box::new(fields),
        })
    } else {
        Ok(DraftRecoveryOutcome::Empty { source })
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc::TryRecvError, time::Instant};

    use super::*;

    fn fields(body: &str) -> ComposeFields {
        ComposeFields {
            body: body.to_string(),
            ..ComposeFields::default()
        }
    }

    fn write_fields(path: &Path, fields: &ComposeFields) {
        std::fs::write(path, serde_json::to_vec(fields).expect("serialize fields"))
            .expect("write fields");
    }

    fn loaded_body(outcome: DraftRecoveryOutcome) -> (DraftRecoverySource, String) {
        match outcome {
            DraftRecoveryOutcome::Loaded { source, fields } => (source, fields.body),
            other => panic!("expected loaded recovery draft, got {other:?}"),
        }
    }

    #[test]
    fn current_recovery_is_preferred_over_legacy() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let current = directory.path().join("current.json");
        let legacy = directory.path().join("legacy.json");
        write_fields(&current, &fields("current"));
        write_fields(&legacy, &fields("legacy"));
        let request = DraftRecoveryRequest::new(1, current.clone(), Some(legacy));

        let (source, body) =
            loaded_body(load_recovery(&request).expect("load current recovery draft"));

        assert_eq!(source, DraftRecoverySource::Current(current));
        assert_eq!(body, "current");
    }

    #[test]
    fn legacy_recovery_is_used_only_when_current_is_missing() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let current = directory.path().join("missing.json");
        let legacy = directory.path().join("legacy.json");
        write_fields(&legacy, &fields("legacy"));
        let request = DraftRecoveryRequest::new(1, current, Some(legacy.clone()));

        let (source, body) =
            loaded_body(load_recovery(&request).expect("load legacy recovery draft"));

        assert_eq!(source, DraftRecoverySource::Legacy(legacy));
        assert!(source.is_legacy());
        assert_eq!(body, "legacy");
    }

    #[test]
    fn malformed_current_draft_is_reported_instead_of_falling_back() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let current = directory.path().join("current.json");
        let legacy = directory.path().join("legacy.json");
        std::fs::write(&current, b"{").expect("write malformed current draft");
        write_fields(&legacy, &fields("legacy"));
        let request = DraftRecoveryRequest::new(1, current.clone(), Some(legacy));

        let error = load_recovery(&request).expect_err("malformed current draft must fail");

        let message = format!("{error:#}");
        assert!(message.contains("parsing recovery draft"), "{message}");
        assert!(
            message.contains(&current.display().to_string()),
            "{message}"
        );
    }

    #[test]
    fn content_free_draft_has_an_explicit_empty_outcome() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let current = directory.path().join("current.json");
        write_fields(&current, &ComposeFields::default());
        let request = DraftRecoveryRequest::new(1, current.clone(), None);

        assert_eq!(
            load_recovery(&request).expect("load empty recovery draft"),
            DraftRecoveryOutcome::Empty {
                source: DraftRecoverySource::Current(current),
            }
        );
    }

    #[test]
    fn bounded_reader_rejects_oversized_recovery() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let current = directory.path().join("current.json");
        std::fs::write(&current, b"12345").expect("write oversized recovery draft");
        let request = DraftRecoveryRequest::new(1, current, None);

        let error =
            load_recovery_with_limit(&request, 4).expect_err("oversized recovery draft must fail");

        assert!(format!("{error:#}").contains("exceeds the 4-byte limit"));
    }

    #[test]
    fn fixture_delay_is_capped_and_stale_or_cancelled_completions_are_rejected() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let current = directory.path().join("current.json");
        write_fields(&current, &fields("delayed"));
        let mut coordinator = DraftRecoveryCoordinator::default();
        let slow_generation = coordinator.begin();
        let request = DraftRecoveryRequest::new(slow_generation, current, None)
            .with_fixture_test_delay(Duration::from_millis(80));
        let started = Instant::now();
        let receiver = spawn(request);
        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));

        let replacement_generation = coordinator.begin();
        let response = receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("receive delayed recovery response");
        assert!(started.elapsed() >= Duration::from_millis(60));
        assert_eq!(response.generation, slow_generation);
        assert!(!coordinator.finish(response.generation));
        assert_eq!(
            coordinator.active_generation(),
            Some(replacement_generation)
        );

        coordinator.cancel();
        assert!(!coordinator.accepts(replacement_generation));
        assert!(!coordinator.finish(replacement_generation));

        let capped = DraftRecoveryRequest::new(9, PathBuf::new(), None)
            .with_fixture_test_delay(Duration::from_secs(60));
        assert_eq!(capped.fixture_test_delay(), MAX_FIXTURE_TEST_DELAY);
    }

    #[test]
    fn fixture_gate_holds_recovery_read_until_released() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let current = directory.path().join("current.json");
        let gate = directory.path().join("release");
        write_fields(&current, &fields("gated"));
        let request =
            DraftRecoveryRequest::new(1, current, None).with_fixture_test_gate(Some(gate.clone()));
        let receiver = spawn(request);

        std::thread::sleep(Duration::from_millis(30));
        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
        std::fs::write(&gate, b"release").expect("release fixture gate");
        let response = receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("receive gated recovery response");
        let (_, body) = loaded_body(response.result.expect("load gated recovery draft"));
        assert_eq!(body, "gated");
    }
}
