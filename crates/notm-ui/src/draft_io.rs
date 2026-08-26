use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::Duration,
};

use anyhow::Context;

use crate::widgets::composer::{
    self, NamedDraftEntry, atomic_write_durable, remove_file_if_present,
};

pub(crate) const MAX_NAMED_DRAFTS: usize = 256;
pub(crate) const MAX_NAMED_DRAFT_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_NAMED_DRAFT_TOTAL_BYTES: usize = 32 * 1024 * 1024;
pub(crate) const MAX_FIXTURE_DELAY: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub(crate) struct NamedDraftLoadRequest {
    pub(crate) generation: u64,
    pub(crate) current_dir: PathBuf,
    pub(crate) legacy_dir: Option<PathBuf>,
    pub(crate) migrate_legacy: bool,
    pub(crate) fixture_delay: Duration,
}

#[derive(Debug)]
pub(crate) struct NamedDraftLoadResult {
    pub(crate) drafts: Vec<NamedDraftEntry>,
    pub(crate) migrated: usize,
}

#[derive(Debug)]
pub(crate) struct WorkerResponse<T> {
    pub(crate) generation: u64,
    pub(crate) result: anyhow::Result<T>,
}

#[derive(Debug, Default)]
pub(crate) struct DraftIoCoordinator {
    next_generation: u64,
    active_generation: Option<u64>,
    completed_generation: Option<u64>,
}

impl DraftIoCoordinator {
    pub(crate) fn begin(&mut self) -> u64 {
        self.next_generation = self.next_generation.saturating_add(1);
        self.active_generation = Some(self.next_generation);
        self.next_generation
    }

    pub(crate) fn accepts(&self, generation: u64) -> bool {
        self.active_generation == Some(generation)
    }

    pub(crate) fn finish(&mut self, generation: u64) -> bool {
        if !self.accepts(generation) {
            return false;
        }
        self.active_generation = None;
        self.completed_generation = Some(generation);
        true
    }

    pub(crate) fn cancel(&mut self) {
        self.active_generation = None;
    }

    pub(crate) fn active_generation(&self) -> Option<u64> {
        self.active_generation
    }

    pub(crate) fn completed_generation(&self) -> Option<u64> {
        self.completed_generation
    }
}

pub(crate) fn spawn_named_draft_load(
    request: NamedDraftLoadRequest,
) -> mpsc::Receiver<WorkerResponse<NamedDraftLoadResult>> {
    let generation = request.generation;
    spawn_worker("notm-named-draft-load", generation, move || {
        if !request.fixture_delay.is_zero() {
            thread::sleep(request.fixture_delay.min(MAX_FIXTURE_DELAY));
        }
        load_named_drafts(&request)
    })
}

pub(crate) fn spawn_worker<T, F>(
    name: &'static str,
    generation: u64,
    work: F,
) -> mpsc::Receiver<WorkerResponse<T>>
where
    T: Send + 'static,
    F: FnOnce() -> anyhow::Result<T> + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    let spawn_result = thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            let _ = tx.send(WorkerResponse {
                generation,
                result: work(),
            });
        });
    if let Err(error) = spawn_result {
        let (tx, replacement_rx) = mpsc::channel();
        let _ = tx.send(WorkerResponse {
            generation,
            result: Err(anyhow::anyhow!("could not spawn {name} worker: {error}")),
        });
        return replacement_rx;
    }
    rx
}

fn load_named_drafts(request: &NamedDraftLoadRequest) -> anyhow::Result<NamedDraftLoadResult> {
    let migrated = if request.migrate_legacy {
        request
            .legacy_dir
            .as_deref()
            .map(|legacy| migrate_legacy_named_drafts(&request.current_dir, legacy))
            .transpose()?
            .unwrap_or(0)
    } else {
        0
    };
    let drafts = scan_named_drafts(&request.current_dir, request.legacy_dir.as_deref())?;
    Ok(NamedDraftLoadResult { drafts, migrated })
}

fn migrate_legacy_named_drafts(dir: &Path, legacy_dir: &Path) -> anyhow::Result<usize> {
    struct LegacyMigration {
        source: PathBuf,
        destination: Option<PathBuf>,
        bytes: Vec<u8>,
    }

    let current_entries = json_entries(dir)?;
    ensure_count(current_entries.len())?;
    let mut current_total_bytes = 0;
    let mut current_by_name = BTreeMap::new();
    let mut occupied_destinations = BTreeSet::new();
    for path in current_entries {
        let filename = path
            .file_name()
            .context("current draft has no filename")?
            .to_os_string();
        let bytes = read_bounded(&path, &mut current_total_bytes)?;
        occupied_destinations.insert(path);
        current_by_name.insert(filename, bytes);
    }

    let legacy_entries = json_entries(legacy_dir)?;
    ensure_count(legacy_entries.len())?;
    let mut legacy_total_bytes = 0;
    let mut migrated_bytes = 0usize;
    let mut migrations = Vec::with_capacity(legacy_entries.len());
    for source in legacy_entries {
        let bytes = read_bounded(&source, &mut legacy_total_bytes)?;
        let filename = source
            .file_name()
            .context("legacy draft has no filename")?
            .to_os_string();
        let destination = if current_by_name.get(&filename) == Some(&bytes) {
            None
        } else if current_by_name.contains_key(&filename) {
            let destination = loop {
                let candidate = dir.join(format!(
                    "legacy-{}-{}",
                    uuid::Uuid::new_v4(),
                    filename.to_string_lossy()
                ));
                if !occupied_destinations.contains(&candidate) {
                    break candidate;
                }
            };
            occupied_destinations.insert(destination.clone());
            Some(destination)
        } else {
            let destination = dir.join(&filename);
            occupied_destinations.insert(destination.clone());
            Some(destination)
        };
        if destination.is_some() {
            migrated_bytes = migrated_bytes
                .checked_add(bytes.len())
                .context("named draft migration byte count overflowed")?;
        }
        migrations.push(LegacyMigration {
            source,
            destination,
            bytes,
        });
    }

    let migrated = migrations
        .iter()
        .filter(|migration| migration.destination.is_some())
        .count();
    let projected_count = current_by_name
        .len()
        .checked_add(migrated)
        .context("named draft migration count overflowed")?;
    anyhow::ensure!(
        projected_count <= MAX_NAMED_DRAFTS,
        "named draft migration would contain {projected_count} JSON files; limit is {MAX_NAMED_DRAFTS}"
    );
    let projected_bytes = current_total_bytes
        .checked_add(migrated_bytes)
        .context("named draft migration projected byte count overflowed")?;
    anyhow::ensure!(
        projected_bytes <= MAX_NAMED_DRAFT_TOTAL_BYTES,
        "named draft migration would use {projected_bytes} bytes; limit is {MAX_NAMED_DRAFT_TOTAL_BYTES}"
    );

    if migrated > 0 {
        composer::ensure_private_directory(dir)?;
    }
    for migration in migrations {
        if let Some(destination) = migration.destination {
            atomic_write_durable(&destination, &migration.bytes)?;
        }
        remove_file_if_present(&migration.source)?;
    }
    Ok(migrated)
}

fn scan_named_drafts(
    dir: &Path,
    legacy_dir: Option<&Path>,
) -> anyhow::Result<Vec<NamedDraftEntry>> {
    let mut paths = json_entries(dir)?;
    if let Some(legacy_dir) = legacy_dir {
        paths.extend(json_entries(legacy_dir)?);
    }
    ensure_count(paths.len())?;

    let mut drafts = Vec::with_capacity(paths.len());
    let mut total_bytes = 0;
    let mut seen = BTreeSet::new();
    for path in paths {
        let bytes = read_bounded(&path, &mut total_bytes)?;
        let fields = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing named draft {}", path.display()))?;
        let duplicate_key = (path.file_name().map(|name| name.to_os_string()), bytes);
        if !seen.insert(duplicate_key) {
            continue;
        }
        let modified = fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .ok();
        drafts.push(NamedDraftEntry {
            modified,
            path,
            fields,
        });
    }
    drafts.sort_by_key(|draft| std::cmp::Reverse(draft.modified));
    Ok(drafts)
}

pub(crate) fn ensure_named_draft_save_fits(
    dir: &Path,
    replacement: Option<&Path>,
    serialized_bytes: usize,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        serialized_bytes <= MAX_NAMED_DRAFT_BYTES,
        "named draft serializes to {serialized_bytes} bytes; limit is {MAX_NAMED_DRAFT_BYTES}"
    );

    let paths = json_entries(dir)?;
    ensure_count(paths.len())?;
    let replacement = replacement
        .map(|path| {
            anyhow::ensure!(
                path.parent() == Some(dir)
                    && path.extension().and_then(|extension| extension.to_str()) == Some("json")
                    && paths.iter().any(|entry| entry == path),
                "replacement named draft {} is not an existing JSON file directly in {}",
                path.display(),
                dir.display()
            );
            anyhow::ensure!(
                fs::metadata(path)
                    .with_context(|| {
                        format!(
                            "reading replacement named draft metadata for {}",
                            path.display()
                        )
                    })?
                    .is_file(),
                "replacement named draft {} is not a file",
                path.display()
            );
            Ok(path)
        })
        .transpose()?;

    let projected_count = paths
        .len()
        .checked_add(usize::from(replacement.is_none()))
        .context("named draft count overflowed")?;
    anyhow::ensure!(
        projected_count <= MAX_NAMED_DRAFTS,
        "named draft save would contain {projected_count} JSON files; limit is {MAX_NAMED_DRAFTS}"
    );

    let mut total_bytes = 0;
    let mut replaced_bytes = 0;
    for path in &paths {
        let bytes = read_bounded(path, &mut total_bytes)?;
        if replacement == Some(path.as_path()) {
            replaced_bytes = bytes.len();
        }
    }
    let projected_bytes = total_bytes
        .checked_sub(replaced_bytes)
        .and_then(|bytes| bytes.checked_add(serialized_bytes))
        .context("named draft projected byte count overflowed")?;
    anyhow::ensure!(
        projected_bytes <= MAX_NAMED_DRAFT_TOTAL_BYTES,
        "named draft save would use {projected_bytes} bytes; limit is {MAX_NAMED_DRAFT_TOTAL_BYTES}"
    );
    Ok(())
}

fn json_entries(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("listing named drafts in {}", dir.display()));
        }
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("listing named drafts in {}", dir.display()))?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("json") {
            paths.push(path);
            if paths.len() > MAX_NAMED_DRAFTS {
                ensure_count(paths.len())?;
            }
        }
    }
    Ok(paths)
}

fn ensure_count(count: usize) -> anyhow::Result<()> {
    anyhow::ensure!(
        count <= MAX_NAMED_DRAFTS,
        "named draft store contains {count} JSON files; limit is {MAX_NAMED_DRAFTS}"
    );
    Ok(())
}

fn read_bounded(path: &Path, total_bytes: &mut usize) -> anyhow::Result<Vec<u8>> {
    let file =
        fs::File::open(path).with_context(|| format!("opening named draft {}", path.display()))?;
    let mut bytes = Vec::new();
    file.take((MAX_NAMED_DRAFT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .with_context(|| format!("reading named draft {}", path.display()))?;
    anyhow::ensure!(
        bytes.len() <= MAX_NAMED_DRAFT_BYTES,
        "named draft {} exceeds the {}-byte limit",
        path.display(),
        MAX_NAMED_DRAFT_BYTES
    );
    *total_bytes = total_bytes
        .checked_add(bytes.len())
        .context("named draft byte count overflowed")?;
    anyhow::ensure!(
        *total_bytes <= MAX_NAMED_DRAFT_TOTAL_BYTES,
        "named draft store exceeds the {}-byte total limit",
        MAX_NAMED_DRAFT_TOTAL_BYTES
    );
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ComposeFields;

    fn fields(subject: &str) -> ComposeFields {
        ComposeFields {
            subject: subject.to_string(),
            body: "body".to_string(),
            ..ComposeFields::default()
        }
    }

    #[test]
    fn bounded_scan_loads_current_and_legacy_once() {
        let root = tempfile::tempdir().expect("tempdir");
        let current = root.path().join("current");
        let legacy = root.path().join("legacy");
        fs::create_dir_all(&current).expect("current dir");
        fs::create_dir_all(&legacy).expect("legacy dir");
        let bytes = serde_json::to_vec(&fields("same")).expect("serialize");
        fs::write(current.join("same.json"), &bytes).expect("current draft");
        fs::write(legacy.join("same.json"), &bytes).expect("legacy draft");

        let loaded = load_named_drafts(&NamedDraftLoadRequest {
            generation: 1,
            current_dir: current,
            legacy_dir: Some(legacy),
            migrate_legacy: false,
            fixture_delay: Duration::ZERO,
        })
        .expect("load drafts");
        assert_eq!(loaded.drafts.len(), 1);
        assert_eq!(loaded.drafts[0].fields.subject, "same");
    }

    #[test]
    fn oversized_named_draft_is_rejected() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("huge.json");
        fs::write(&path, vec![b'x'; MAX_NAMED_DRAFT_BYTES + 1]).expect("write huge draft");
        let error = scan_named_drafts(root.path(), None).expect_err("oversized draft must fail");
        assert!(error.to_string().contains("exceeds"), "{error:#}");
    }

    #[test]
    fn coordinator_rejects_stale_completion() {
        let mut coordinator = DraftIoCoordinator::default();
        let old = coordinator.begin();
        let current = coordinator.begin();
        assert!(!coordinator.accepts(old));
        assert!(coordinator.accepts(current));
        assert!(!coordinator.finish(old));
        assert!(coordinator.finish(current));
        assert_eq!(coordinator.completed_generation(), Some(current));
    }

    #[test]
    fn legacy_migration_rejects_combined_count_without_mutating_either_store() {
        let root = tempfile::tempdir().expect("tempdir");
        let current = root.path().join("current");
        let legacy = root.path().join("legacy");
        fs::create_dir_all(&current).expect("current dir");
        fs::create_dir_all(&legacy).expect("legacy dir");
        let existing = serde_json::to_vec(&fields("existing")).expect("serialize existing");
        for index in 0..MAX_NAMED_DRAFTS {
            fs::write(current.join(format!("draft-{index:03}.json")), &existing)
                .expect("write current draft");
        }
        fs::write(legacy.join("draft-000.json"), &existing).expect("write duplicate legacy draft");
        let newcomer = serde_json::to_vec(&fields("newcomer")).expect("serialize newcomer");
        fs::write(legacy.join("newcomer.json"), &newcomer)
            .expect("write nonduplicate legacy draft");

        let error = migrate_legacy_named_drafts(&current, &legacy)
            .expect_err("combined count must reject migration");
        assert!(error.to_string().contains("would contain 257"), "{error:#}");
        assert_eq!(json_entries(&current).expect("list current").len(), 256);
        assert_eq!(
            fs::read(legacy.join("draft-000.json")).expect("read duplicate after rejection"),
            existing
        );
        assert_eq!(
            fs::read(legacy.join("newcomer.json")).expect("read newcomer after rejection"),
            newcomer
        );
        assert_eq!(
            scan_named_drafts(&current, None)
                .expect("last good current store remains readable")
                .len(),
            MAX_NAMED_DRAFTS
        );
    }

    #[test]
    fn legacy_migration_rejects_combined_bytes_without_mutating_either_store() {
        let root = tempfile::tempdir().expect("tempdir");
        let current = root.path().join("current");
        let legacy = root.path().join("legacy");
        fs::create_dir_all(&current).expect("current dir");
        fs::create_dir_all(&legacy).expect("legacy dir");

        let mut full_sized_fields = fields("capacity");
        full_sized_fields.body.clear();
        let empty_size = serde_json::to_vec(&full_sized_fields)
            .expect("serialize empty draft")
            .len();
        full_sized_fields.body = "x".repeat(MAX_NAMED_DRAFT_BYTES - empty_size);
        let full_sized = serde_json::to_vec(&full_sized_fields).expect("serialize full draft");
        assert_eq!(full_sized.len(), MAX_NAMED_DRAFT_BYTES);
        let full_draft_count = MAX_NAMED_DRAFT_TOTAL_BYTES / MAX_NAMED_DRAFT_BYTES;
        for index in 0..full_draft_count {
            fs::write(current.join(format!("draft-{index:03}.json")), &full_sized)
                .expect("write current draft");
        }
        let newcomer = serde_json::to_vec(&fields("newcomer")).expect("serialize newcomer");
        fs::write(legacy.join("newcomer.json"), &newcomer)
            .expect("write nonduplicate legacy draft");

        let error = migrate_legacy_named_drafts(&current, &legacy)
            .expect_err("combined bytes must reject migration");
        assert!(error.to_string().contains("would use"), "{error:#}");
        assert_eq!(
            json_entries(&current).expect("list current").len(),
            full_draft_count
        );
        assert_eq!(
            fs::read(legacy.join("newcomer.json")).expect("read newcomer after rejection"),
            newcomer
        );
        assert_eq!(
            scan_named_drafts(&current, None)
                .expect("last good current store remains readable")
                .len(),
            full_draft_count
        );
    }
}
