use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, Read},
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
const MAX_NAMED_DRAFT_WARNING_DETAILS: usize = 3;
const MAX_NAMED_DRAFT_WARNING_BYTES: usize = 1024;

type NamedDraftReader = Box<dyn Read>;
type NamedDraftReaderFactory<'a> = dyn FnMut(&Path) -> io::Result<NamedDraftReader> + 'a;

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
    pub(crate) warning: Option<String>,
}

#[derive(Debug, Default)]
struct NamedDraftLoadWarnings {
    rejected_entries: usize,
    migration_failed: bool,
    details: Vec<String>,
}

impl NamedDraftLoadWarnings {
    fn reject(&mut self, path: &Path, error: impl std::fmt::Display) {
        self.rejected_entries = self.rejected_entries.saturating_add(1);
        self.push_detail(format!("{}: {error}", path.display()));
    }

    fn migration_failed(&mut self, error: impl std::fmt::Display) {
        self.migration_failed = true;
        self.push_detail(format!("legacy migration: {error}"));
    }

    fn append(&mut self, mut other: Self) {
        self.rejected_entries = self.rejected_entries.saturating_add(other.rejected_entries);
        self.migration_failed |= other.migration_failed;
        for detail in other.details.drain(..) {
            self.push_detail(detail);
        }
    }

    fn push_detail(&mut self, detail: String) {
        if self.details.len() < MAX_NAMED_DRAFT_WARNING_DETAILS {
            self.details
                .push(truncate_utf8(&detail, MAX_NAMED_DRAFT_WARNING_BYTES / 4));
        }
    }

    fn into_message(self) -> Option<String> {
        let issue_count = self
            .rejected_entries
            .saturating_add(usize::from(self.migration_failed));
        if issue_count == 0 {
            return None;
        }

        let mut message = match (self.rejected_entries, self.migration_failed) {
            (0, true) => "legacy named-draft migration could not complete".to_string(),
            (count, false) => format!(
                "rejected {count} unreadable, oversized, or malformed named draft{}",
                if count == 1 { "" } else { "s" }
            ),
            (count, true) => format!(
                "legacy named-draft migration could not complete and {count} named draft{} \
                 were rejected",
                if count == 1 { "" } else { "s" }
            ),
        };
        if !self.details.is_empty() {
            message.push_str(": ");
            message.push_str(&self.details.join("; "));
        }
        let omitted = issue_count.saturating_sub(self.details.len());
        if omitted > 0 {
            message.push_str(&format!("; {omitted} additional issue(s) omitted"));
        }
        Some(truncate_utf8(&message, MAX_NAMED_DRAFT_WARNING_BYTES))
    }
}

#[derive(Debug)]
struct NamedDraftScanResult {
    drafts: Vec<NamedDraftEntry>,
    warnings: NamedDraftLoadWarnings,
}

#[derive(Debug)]
struct LegacyMigrationError {
    error: anyhow::Error,
    recoverable_entry: bool,
}

impl LegacyMigrationError {
    fn rejected_entry(error: anyhow::Error) -> Self {
        Self {
            error,
            recoverable_entry: true,
        }
    }

    fn fatal(error: anyhow::Error) -> Self {
        Self {
            error,
            recoverable_entry: false,
        }
    }

    fn into_anyhow(self) -> anyhow::Error {
        self.error
    }
}

impl std::fmt::Display for LegacyMigrationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(formatter)
    }
}

impl std::error::Error for LegacyMigrationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.error.source()
    }
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
    active_migration: bool,
    completed_generation: Option<u64>,
}

impl DraftIoCoordinator {
    pub(crate) fn begin(&mut self, migrate_legacy: bool) -> Option<u64> {
        // A legacy migration is the only named-draft load that mutates the
        // store. Its worker is deliberately not cancellable once started, so
        // a newer refresh must not replace the generation that gates draft
        // saves, deletes, and accepted-send cleanup for its full lifetime.
        if self.active_migration {
            return None;
        }
        self.next_generation = self.next_generation.saturating_add(1);
        self.active_generation = Some(self.next_generation);
        self.active_migration = migrate_legacy;
        Some(self.next_generation)
    }

    pub(crate) fn accepts(&self, generation: u64) -> bool {
        self.active_generation == Some(generation)
    }

    pub(crate) fn finish(&mut self, generation: u64) -> bool {
        if !self.accepts(generation) {
            return false;
        }
        self.active_generation = None;
        self.active_migration = false;
        self.completed_generation = Some(generation);
        true
    }

    pub(crate) fn cancel(&mut self) {
        self.active_generation = None;
        self.active_migration = false;
    }

    pub(crate) fn active_generation(&self) -> Option<u64> {
        self.active_generation
    }

    pub(crate) fn completed_generation(&self) -> Option<u64> {
        self.completed_generation
    }

    pub(crate) fn migration_in_progress(&self) -> bool {
        self.active_generation.is_some() && self.active_migration
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
    let mut warnings = NamedDraftLoadWarnings::default();
    let migrated = if request.migrate_legacy {
        match request
            .legacy_dir
            .as_deref()
            .map(|legacy| migrate_legacy_named_drafts(&request.current_dir, legacy))
            .transpose()
        {
            Ok(migrated) => migrated.unwrap_or(0),
            Err(error) if error.recoverable_entry => {
                // Migration preflights every source and only replaces files
                // atomically. A failed migration is therefore safe to leave
                // in place and scan from both locations, so one bad entry
                // does not hide unrelated valid drafts.
                warnings.migration_failed(format!("{error:#}"));
                0
            }
            Err(error) => return Err(error.into_anyhow()),
        }
    } else {
        0
    };
    let scan = scan_named_drafts(&request.current_dir, request.legacy_dir.as_deref())?;
    warnings.append(scan.warnings);
    Ok(NamedDraftLoadResult {
        drafts: scan.drafts,
        migrated,
        warning: warnings.into_message(),
    })
}

fn migrate_legacy_named_drafts(
    dir: &Path,
    legacy_dir: &Path,
) -> Result<usize, LegacyMigrationError> {
    let mut reader_factory = open_named_draft_reader;
    migrate_legacy_named_drafts_with_reader(dir, legacy_dir, &mut reader_factory)
}

fn migrate_legacy_named_drafts_with_reader(
    dir: &Path,
    legacy_dir: &Path,
    reader_factory: &mut NamedDraftReaderFactory<'_>,
) -> Result<usize, LegacyMigrationError> {
    struct LegacyMigration {
        source: PathBuf,
        destination: Option<PathBuf>,
        bytes: Vec<u8>,
    }

    let current_entries = json_entries(dir).map_err(LegacyMigrationError::fatal)?;
    ensure_count(current_entries.len()).map_err(LegacyMigrationError::fatal)?;
    let mut current_total_bytes = 0;
    let mut current_by_name = BTreeMap::new();
    let mut occupied_destinations = BTreeSet::new();
    for path in current_entries {
        let filename = path
            .file_name()
            .context("current draft has no filename")
            .map_err(LegacyMigrationError::fatal)?
            .to_os_string();
        let bytes = read_bounded_for_migration(&path, &mut current_total_bytes, reader_factory)?;
        occupied_destinations.insert(path);
        current_by_name.insert(filename, bytes);
    }

    let legacy_entries = json_entries(legacy_dir).map_err(LegacyMigrationError::fatal)?;
    ensure_count(legacy_entries.len()).map_err(LegacyMigrationError::fatal)?;
    let mut legacy_total_bytes = 0;
    let mut migrated_bytes = 0usize;
    let mut preserved_legacy_bytes = 0usize;
    let mut preserved_legacy_count = 0usize;
    let mut migrations = Vec::with_capacity(legacy_entries.len());
    for source in legacy_entries {
        let total_before = legacy_total_bytes;
        let bytes =
            match read_bounded_for_migration(&source, &mut legacy_total_bytes, reader_factory) {
                Ok(bytes) => bytes,
                Err(error) if error.recoverable_entry => {
                    preserved_legacy_bytes = preserved_legacy_bytes
                        .checked_add(legacy_total_bytes.saturating_sub(total_before))
                        .ok_or_else(|| {
                            LegacyMigrationError::fatal(anyhow::anyhow!(
                                "named draft migration preserved byte count overflowed"
                            ))
                        })?;
                    preserved_legacy_count =
                        preserved_legacy_count.checked_add(1).ok_or_else(|| {
                            LegacyMigrationError::fatal(anyhow::anyhow!(
                                "named draft migration preserved count overflowed"
                            ))
                        })?;
                    continue;
                }
                Err(error) => return Err(error),
            };
        if serde_json::from_slice::<crate::model::ComposeFields>(&bytes).is_err() {
            preserved_legacy_bytes =
                preserved_legacy_bytes
                    .checked_add(bytes.len())
                    .ok_or_else(|| {
                        LegacyMigrationError::fatal(anyhow::anyhow!(
                            "named draft migration preserved byte count overflowed"
                        ))
                    })?;
            preserved_legacy_count = preserved_legacy_count.checked_add(1).ok_or_else(|| {
                LegacyMigrationError::fatal(anyhow::anyhow!(
                    "named draft migration preserved count overflowed"
                ))
            })?;
            continue;
        }
        let filename = source
            .file_name()
            .context("legacy draft has no filename")
            .map_err(LegacyMigrationError::fatal)?
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
            migrated_bytes = migrated_bytes.checked_add(bytes.len()).ok_or_else(|| {
                LegacyMigrationError::fatal(anyhow::anyhow!(
                    "named draft migration byte count overflowed"
                ))
            })?;
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
        .and_then(|count| count.checked_add(preserved_legacy_count))
        .ok_or_else(|| {
            LegacyMigrationError::fatal(anyhow::anyhow!("named draft migration count overflowed"))
        })?;
    if projected_count > MAX_NAMED_DRAFTS {
        return Err(LegacyMigrationError::fatal(anyhow::anyhow!(
            "named draft migration would contain {projected_count} JSON files; limit is {MAX_NAMED_DRAFTS}"
        )));
    }
    let projected_bytes = current_total_bytes
        .checked_add(migrated_bytes)
        .and_then(|bytes| bytes.checked_add(preserved_legacy_bytes))
        .ok_or_else(|| {
            LegacyMigrationError::fatal(anyhow::anyhow!(
                "named draft migration projected byte count overflowed"
            ))
        })?;
    if projected_bytes > MAX_NAMED_DRAFT_TOTAL_BYTES {
        return Err(LegacyMigrationError::fatal(anyhow::anyhow!(
            "named draft migration would use {projected_bytes} bytes; limit is {MAX_NAMED_DRAFT_TOTAL_BYTES}"
        )));
    }

    if migrated > 0 {
        composer::ensure_private_directory(dir).map_err(LegacyMigrationError::fatal)?;
    }
    for migration in migrations {
        if let Some(destination) = migration.destination {
            atomic_write_durable(&destination, &migration.bytes)
                .map_err(LegacyMigrationError::fatal)?;
        }
        remove_file_if_present(&migration.source).map_err(LegacyMigrationError::fatal)?;
    }
    Ok(migrated)
}

fn scan_named_drafts(
    dir: &Path,
    legacy_dir: Option<&Path>,
) -> anyhow::Result<NamedDraftScanResult> {
    let mut reader_factory = open_named_draft_reader;
    scan_named_drafts_with_reader(dir, legacy_dir, &mut reader_factory)
}

fn scan_named_drafts_with_reader(
    dir: &Path,
    legacy_dir: Option<&Path>,
    reader_factory: &mut NamedDraftReaderFactory<'_>,
) -> anyhow::Result<NamedDraftScanResult> {
    let mut paths = json_entries(dir)?;
    if let Some(legacy_dir) = legacy_dir {
        paths.extend(json_entries(legacy_dir)?);
    }
    ensure_count(paths.len())?;

    let mut drafts = Vec::with_capacity(paths.len());
    let mut total_bytes = 0usize;
    let mut seen = BTreeSet::new();
    let mut warnings = NamedDraftLoadWarnings::default();
    for path in paths {
        let metadata = match fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => metadata,
            Ok(_) => {
                warnings.reject(&path, "entry is not a regular file");
                continue;
            }
            Err(error) => {
                warnings.reject(&path, format!("reading metadata: {error}"));
                continue;
            }
        };
        let bytes = match read_named_draft(&path, reader_factory) {
            Ok(bytes) => bytes,
            Err(error) => {
                warnings.reject(&path, format!("{error:#}"));
                continue;
            }
        };
        charge_named_draft_bytes(&mut total_bytes, bytes.len())?;
        if bytes.len() > MAX_NAMED_DRAFT_BYTES {
            warnings.reject(
                &path,
                format!("exceeds the {MAX_NAMED_DRAFT_BYTES}-byte per-entry limit"),
            );
            continue;
        }
        let fields = match serde_json::from_slice(&bytes) {
            Ok(fields) => fields,
            Err(error) => {
                warnings.reject(&path, format!("parsing JSON: {error}"));
                continue;
            }
        };
        let duplicate_key = (path.file_name().map(|name| name.to_os_string()), bytes);
        if !seen.insert(duplicate_key) {
            continue;
        }
        let modified = metadata.modified().ok();
        drafts.push(NamedDraftEntry {
            modified,
            path,
            fields,
        });
    }
    drafts.sort_by_key(|draft| std::cmp::Reverse(draft.modified));
    Ok(NamedDraftScanResult { drafts, warnings })
}

pub(crate) fn ensure_named_draft_save_fits(
    dir: &Path,
    legacy_dir: Option<&Path>,
    replacement: Option<&Path>,
    serialized_bytes: usize,
) -> anyhow::Result<()> {
    let mut reader_factory = open_named_draft_reader;
    ensure_named_draft_save_fits_with_reader(
        dir,
        legacy_dir,
        replacement,
        serialized_bytes,
        &mut reader_factory,
    )
}

fn ensure_named_draft_save_fits_with_reader(
    dir: &Path,
    legacy_dir: Option<&Path>,
    replacement: Option<&Path>,
    serialized_bytes: usize,
    reader_factory: &mut NamedDraftReaderFactory<'_>,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        serialized_bytes <= MAX_NAMED_DRAFT_BYTES,
        "named draft serializes to {serialized_bytes} bytes; limit is {MAX_NAMED_DRAFT_BYTES}"
    );

    let paths = json_entries(dir)?;
    let legacy_paths = match legacy_dir {
        Some(legacy_dir) if legacy_dir != dir => json_entries(legacy_dir)?,
        _ => Vec::new(),
    };
    let existing_count = paths
        .len()
        .checked_add(legacy_paths.len())
        .context("named draft count overflowed")?;
    ensure_count(existing_count)?;
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

    let projected_count = existing_count
        .checked_add(usize::from(replacement.is_none()))
        .context("named draft count overflowed")?;
    anyhow::ensure!(
        projected_count <= MAX_NAMED_DRAFTS,
        "named draft save would contain {projected_count} JSON files; limit is {MAX_NAMED_DRAFTS}"
    );

    let mut total_bytes = 0;
    let mut replaced_bytes = 0;
    for path in &paths {
        let bytes = read_bounded(path, &mut total_bytes, reader_factory)?;
        if replacement == Some(path.as_path()) {
            replaced_bytes = bytes.len();
        }
    }
    for path in &legacy_paths {
        // Legacy entries rejected by migration remain visible to the startup
        // scan and still occupy the shared physical count and byte budgets.
        // Keep per-entry read failures recoverable, as the scan does, and do
        // not charge any bytes from a failed partial read. Successfully read
        // malformed and oversized entries are charged before they are skipped.
        if let Ok(bytes) = read_named_draft(path, reader_factory) {
            charge_named_draft_bytes(&mut total_bytes, bytes.len())?;
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

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let suffix = "…";
    let mut end = max_bytes.saturating_sub(suffix.len()).min(value.len());
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}{suffix}", &value[..end])
}

fn read_bounded_for_migration(
    path: &Path,
    total_bytes: &mut usize,
    reader_factory: &mut NamedDraftReaderFactory<'_>,
) -> Result<Vec<u8>, LegacyMigrationError> {
    let bytes =
        read_named_draft(path, reader_factory).map_err(LegacyMigrationError::rejected_entry)?;
    charge_named_draft_bytes(total_bytes, bytes.len()).map_err(LegacyMigrationError::fatal)?;
    if bytes.len() > MAX_NAMED_DRAFT_BYTES {
        return Err(LegacyMigrationError::rejected_entry(anyhow::anyhow!(
            "named draft {} exceeds the {}-byte limit",
            path.display(),
            MAX_NAMED_DRAFT_BYTES
        )));
    }
    Ok(bytes)
}

fn open_named_draft_reader(path: &Path) -> io::Result<NamedDraftReader> {
    fs::File::open(path).map(|file| Box::new(file) as NamedDraftReader)
}

fn read_named_draft(
    path: &Path,
    reader_factory: &mut NamedDraftReaderFactory<'_>,
) -> anyhow::Result<Vec<u8>> {
    let reader =
        reader_factory(path).with_context(|| format!("opening named draft {}", path.display()))?;
    let mut bytes = Vec::new();
    reader
        .take((MAX_NAMED_DRAFT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .with_context(|| format!("reading named draft {}", path.display()))?;
    Ok(bytes)
}

fn charge_named_draft_bytes(total_bytes: &mut usize, bytes: usize) -> anyhow::Result<()> {
    let updated_total = total_bytes
        .checked_add(bytes)
        .context("named draft byte count overflowed")?;
    anyhow::ensure!(
        updated_total <= MAX_NAMED_DRAFT_TOTAL_BYTES,
        "named draft store exceeds the {}-byte total limit",
        MAX_NAMED_DRAFT_TOTAL_BYTES
    );
    *total_bytes = updated_total;
    Ok(())
}

fn read_bounded(
    path: &Path,
    total_bytes: &mut usize,
    reader_factory: &mut NamedDraftReaderFactory<'_>,
) -> anyhow::Result<Vec<u8>> {
    let bytes = read_named_draft(path, reader_factory)?;
    anyhow::ensure!(
        bytes.len() <= MAX_NAMED_DRAFT_BYTES,
        "named draft {} exceeds the {}-byte limit",
        path.display(),
        MAX_NAMED_DRAFT_BYTES
    );
    charge_named_draft_bytes(total_bytes, bytes.len())?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ComposeFields;
    use std::{cell::Cell, io::Cursor, rc::Rc};

    struct PartialThenError {
        remaining: usize,
        emitted: Rc<Cell<usize>>,
    }

    impl Read for PartialThenError {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.remaining == 0 {
                return Err(io::Error::other("injected read failure after partial data"));
            }
            let count = self.remaining.min(buffer.len());
            buffer[..count].fill(b'p');
            self.remaining -= count;
            self.emitted.set(self.emitted.get().saturating_add(count));
            Ok(count)
        }
    }

    fn fields(subject: &str) -> ComposeFields {
        ComposeFields {
            subject: subject.to_string(),
            body: "body".to_string(),
            ..ComposeFields::default()
        }
    }

    fn create_partial_read_budget_fixture(dir: &Path) -> Vec<u8> {
        let valid = serde_json::to_vec(&fields("valid-after-partial-error"))
            .expect("serialize valid draft");
        let full_entries = MAX_NAMED_DRAFT_TOTAL_BYTES / MAX_NAMED_DRAFT_BYTES - 1;
        let tail_bytes = MAX_NAMED_DRAFT_BYTES
            .checked_sub(valid.len())
            .expect("valid draft fits one entry");
        assert_eq!(
            full_entries * MAX_NAMED_DRAFT_BYTES + tail_bytes + valid.len(),
            MAX_NAMED_DRAFT_TOTAL_BYTES
        );

        for index in 0..full_entries {
            fs::write(
                dir.join(format!("malformed-full-{index:02}.json")),
                b"fixture",
            )
            .expect("write synthetic full entry placeholder");
        }
        fs::write(dir.join("malformed-tail.json"), b"fixture")
            .expect("write synthetic tail entry placeholder");
        fs::write(dir.join("partial.json"), b"fixture")
            .expect("write partial-read entry placeholder");
        fs::write(dir.join("valid.json"), b"fixture").expect("write valid entry placeholder");
        valid
    }

    fn partial_read_budget_reader(
        valid: Vec<u8>,
        partial_emitted: Rc<Cell<usize>>,
    ) -> impl FnMut(&Path) -> io::Result<NamedDraftReader> {
        let tail_bytes = MAX_NAMED_DRAFT_BYTES - valid.len();
        move |path| {
            let filename = path
                .file_name()
                .and_then(|filename| filename.to_str())
                .ok_or_else(|| io::Error::other("synthetic entry has no UTF-8 filename"))?;
            let reader: NamedDraftReader = match filename {
                "valid.json" => Box::new(Cursor::new(valid.clone())),
                "malformed-tail.json" => Box::new(io::repeat(b'x').take(tail_bytes as u64)),
                "partial.json" => Box::new(PartialThenError {
                    remaining: MAX_NAMED_DRAFT_BYTES,
                    emitted: Rc::clone(&partial_emitted),
                }),
                filename if filename.starts_with("malformed-full-") => {
                    Box::new(io::repeat(b'x').take(MAX_NAMED_DRAFT_BYTES as u64))
                }
                _ => {
                    return Err(io::Error::other(format!(
                        "unexpected synthetic entry {filename}"
                    )));
                }
            };
            Ok(reader)
        }
    }

    #[derive(Clone)]
    enum SyntheticLegacyRead {
        Bytes(usize),
        PartialError {
            bytes: usize,
            emitted: Rc<Cell<usize>>,
        },
    }

    fn create_named_draft_placeholders(dir: &Path, count: usize) {
        for index in 0..count {
            fs::write(dir.join(format!("current-{index:02}.json")), b"fixture")
                .expect("write current draft placeholder");
        }
    }

    fn save_preflight_budget_reader(
        current_dir: PathBuf,
        current_bytes: usize,
        legacy_path: PathBuf,
        legacy_read: SyntheticLegacyRead,
    ) -> impl FnMut(&Path) -> io::Result<NamedDraftReader> {
        move |path| {
            if path.parent() == Some(current_dir.as_path()) {
                return Ok(Box::new(io::repeat(b'x').take(current_bytes as u64)));
            }
            if path != legacy_path {
                return Err(io::Error::other(format!(
                    "unexpected synthetic entry {}",
                    path.display()
                )));
            }
            match &legacy_read {
                SyntheticLegacyRead::Bytes(bytes) => {
                    Ok(Box::new(io::repeat(b'x').take(*bytes as u64)))
                }
                SyntheticLegacyRead::PartialError { bytes, emitted } => {
                    Ok(Box::new(PartialThenError {
                        remaining: *bytes,
                        emitted: Rc::clone(emitted),
                    }))
                }
            }
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
        assert_eq!(loaded.warning, None);
    }

    #[test]
    fn valid_and_malformed_named_drafts_load_with_a_bounded_warning() {
        let root = tempfile::tempdir().expect("tempdir");
        fs::write(
            root.path().join("valid.json"),
            serde_json::to_vec(&fields("valid")).expect("serialize valid draft"),
        )
        .expect("write valid draft");
        fs::write(root.path().join("malformed.json"), b"{truncated")
            .expect("write malformed draft");
        for index in 0..(MAX_NAMED_DRAFT_WARNING_DETAILS + 2) {
            fs::write(
                root.path().join(format!("malformed-{index}.json")),
                b"not-json",
            )
            .expect("write extra malformed draft");
        }

        let scan = scan_named_drafts(root.path(), None).expect("scan with rejected entries");
        assert_eq!(scan.drafts.len(), 1);
        assert_eq!(scan.drafts[0].fields.subject, "valid");
        let warning = scan.warnings.into_message().expect("rejection warning");
        assert!(warning.contains("rejected 6"), "{warning}");
        assert!(warning.contains("additional issue(s) omitted"), "{warning}");
        assert!(warning.len() <= MAX_NAMED_DRAFT_WARNING_BYTES);
    }

    #[test]
    fn valid_unreadable_and_oversized_named_drafts_preserve_the_valid_list() {
        let root = tempfile::tempdir().expect("tempdir");
        fs::write(
            root.path().join("valid.json"),
            serde_json::to_vec(&fields("valid")).expect("serialize valid draft"),
        )
        .expect("write valid draft");
        fs::create_dir(root.path().join("unreadable.json")).expect("unreadable entry");
        fs::write(
            root.path().join("oversized.json"),
            vec![b'x'; MAX_NAMED_DRAFT_BYTES + 1],
        )
        .expect("write oversized draft");

        let scan = scan_named_drafts(root.path(), None).expect("scan with rejected entries");
        assert_eq!(scan.drafts.len(), 1);
        assert_eq!(scan.drafts[0].fields.subject, "valid");
        let warning = scan.warnings.into_message().expect("rejection warning");
        assert!(warning.contains("rejected 2"), "{warning}");
        assert!(warning.contains("unreadable.json"), "{warning}");
        assert!(warning.contains("oversized.json"), "{warning}");
    }

    #[test]
    fn rejected_oversized_entries_still_enforce_the_aggregate_read_budget() {
        let root = tempfile::tempdir().expect("tempdir");
        let oversized = vec![b'x'; MAX_NAMED_DRAFT_BYTES + 1];
        let entries_to_exceed_total = MAX_NAMED_DRAFT_TOTAL_BYTES / (MAX_NAMED_DRAFT_BYTES + 1) + 1;
        for index in 0..entries_to_exceed_total {
            fs::write(
                root.path().join(format!("oversized-{index}.json")),
                &oversized,
            )
            .expect("write oversized draft");
        }

        let error = scan_named_drafts(root.path(), None)
            .expect_err("aggregate rejected-entry reads must stay bounded");
        assert!(error.to_string().contains("total limit"), "{error:#}");
    }

    #[test]
    fn malformed_in_limit_entries_count_toward_the_aggregate_read_budget() {
        let root = tempfile::tempdir().expect("tempdir");
        let malformed = vec![b'x'; MAX_NAMED_DRAFT_BYTES];
        let entries_to_exceed_total = MAX_NAMED_DRAFT_TOTAL_BYTES / MAX_NAMED_DRAFT_BYTES + 1;
        for index in 0..entries_to_exceed_total {
            fs::write(
                root.path().join(format!("malformed-{index}.json")),
                &malformed,
            )
            .expect("write malformed draft");
        }

        let error = scan_named_drafts(root.path(), None)
            .expect_err("malformed entry reads must enforce the aggregate limit");
        assert!(error.to_string().contains("total limit"), "{error:#}");
    }

    #[test]
    fn partial_read_error_does_not_charge_scan_budget_or_hide_valid_draft() {
        let root = tempfile::tempdir().expect("tempdir");
        let valid = create_partial_read_budget_fixture(root.path());
        let partial_emitted = Rc::new(Cell::new(0));
        let mut reader_factory = partial_read_budget_reader(valid, Rc::clone(&partial_emitted));

        let scan = scan_named_drafts_with_reader(root.path(), None, &mut reader_factory)
            .expect("partial read bytes must not consume aggregate scan budget");

        assert!(
            partial_emitted.get() > 0,
            "injected reader emitted no bytes"
        );
        assert_eq!(scan.drafts.len(), 1);
        assert_eq!(scan.drafts[0].fields.subject, "valid-after-partial-error");
        assert_eq!(scan.warnings.rejected_entries, 17);
    }

    #[test]
    fn partial_read_error_does_not_charge_migration_budget_or_block_valid_draft() {
        let root = tempfile::tempdir().expect("tempdir");
        let current = root.path().join("current");
        let legacy = root.path().join("legacy");
        fs::create_dir_all(&current).expect("current dir");
        fs::create_dir_all(&legacy).expect("legacy dir");
        let valid = create_partial_read_budget_fixture(&legacy);
        let partial_emitted = Rc::new(Cell::new(0));
        let mut reader_factory = partial_read_budget_reader(valid, Rc::clone(&partial_emitted));

        let migrated =
            migrate_legacy_named_drafts_with_reader(&current, &legacy, &mut reader_factory)
                .expect("partial read bytes must not consume aggregate migration budget");

        assert!(
            partial_emitted.get() > 0,
            "injected reader emitted no bytes"
        );
        assert_eq!(migrated, 1);
        let migrated_fields: ComposeFields = serde_json::from_slice(
            &fs::read(current.join("valid.json")).expect("read migrated valid draft"),
        )
        .expect("parse migrated valid draft");
        assert_eq!(migrated_fields.subject, "valid-after-partial-error");
        assert!(legacy.join("partial.json").exists());
    }

    #[test]
    fn save_preflight_counts_malformed_legacy_entry_retained_after_migration() {
        let root = tempfile::tempdir().expect("tempdir");
        let current = root.path().join("current");
        let legacy = root.path().join("legacy");
        fs::create_dir_all(&current).expect("current dir");
        fs::create_dir_all(&legacy).expect("legacy dir");
        let valid = serde_json::to_vec(&fields("current")).expect("serialize current draft");
        for index in 0..(MAX_NAMED_DRAFTS - 1) {
            fs::write(current.join(format!("current-{index:03}.json")), &valid)
                .expect("write current draft");
        }
        let malformed_path = legacy.join("retained-malformed.json");
        fs::write(&malformed_path, b"{truncated").expect("write malformed legacy draft");

        let loaded = load_named_drafts(&NamedDraftLoadRequest {
            generation: 1,
            current_dir: current.clone(),
            legacy_dir: Some(legacy.clone()),
            migrate_legacy: true,
            fixture_delay: Duration::ZERO,
        })
        .expect("load current drafts while retaining malformed legacy entry");
        assert_eq!(loaded.migrated, 0);
        assert_eq!(loaded.drafts.len(), MAX_NAMED_DRAFTS - 1);
        let warning = loaded.warning.expect("malformed legacy warning");
        assert!(warning.contains("retained-malformed.json"), "{warning}");

        let error = ensure_named_draft_save_fits(&current, Some(&legacy), None, valid.len())
            .expect_err("retained legacy entry must count against a new save");
        assert!(error.to_string().contains("would contain 257"), "{error:#}");
        assert!(malformed_path.is_file());
        assert_eq!(json_entries(&current).expect("list current").len(), 255);
    }

    #[test]
    fn save_preflight_counts_unreadable_legacy_entry_and_allows_current_replacement() {
        let root = tempfile::tempdir().expect("tempdir");
        let current = root.path().join("current");
        let legacy = root.path().join("legacy");
        fs::create_dir_all(&current).expect("current dir");
        fs::create_dir_all(&legacy).expect("legacy dir");
        let valid = serde_json::to_vec(&fields("current")).expect("serialize current draft");
        for index in 0..(MAX_NAMED_DRAFTS - 1) {
            fs::write(current.join(format!("current-{index:03}.json")), &valid)
                .expect("write current draft");
        }
        let unreadable_path = legacy.join("retained-unreadable.json");
        fs::create_dir(&unreadable_path).expect("create deterministic unreadable entry");

        let loaded = load_named_drafts(&NamedDraftLoadRequest {
            generation: 1,
            current_dir: current.clone(),
            legacy_dir: Some(legacy.clone()),
            migrate_legacy: true,
            fixture_delay: Duration::ZERO,
        })
        .expect("load current drafts while retaining unreadable legacy entry");
        assert_eq!(loaded.migrated, 0);
        assert_eq!(loaded.drafts.len(), MAX_NAMED_DRAFTS - 1);
        let warning = loaded.warning.expect("unreadable legacy warning");
        assert!(warning.contains("retained-unreadable.json"), "{warning}");

        let error = ensure_named_draft_save_fits(&current, Some(&legacy), None, valid.len())
            .expect_err("retained unreadable entry must count against a new save");
        assert!(error.to_string().contains("would contain 257"), "{error:#}");

        let replacement = current.join("current-000.json");
        ensure_named_draft_save_fits(&current, Some(&legacy), Some(&replacement), valid.len())
            .expect("replacement should remain allowed at the combined physical count cap");
        assert!(unreadable_path.is_dir());
    }

    #[test]
    fn save_preflight_charges_malformed_legacy_bytes_retained_after_migration() {
        let root = tempfile::tempdir().expect("tempdir");
        let current = root.path().join("current");
        let legacy = root.path().join("legacy");
        fs::create_dir_all(&current).expect("current dir");
        fs::create_dir_all(&legacy).expect("legacy dir");
        let current_count = MAX_NAMED_DRAFT_TOTAL_BYTES / MAX_NAMED_DRAFT_BYTES - 1;
        create_named_draft_placeholders(&current, current_count);
        let legacy_path = legacy.join("retained-malformed.json");
        fs::write(&legacy_path, b"fixture").expect("write legacy draft placeholder");

        let legacy_read = SyntheticLegacyRead::Bytes(MAX_NAMED_DRAFT_BYTES);
        let mut migration_reader = save_preflight_budget_reader(
            current.clone(),
            MAX_NAMED_DRAFT_BYTES,
            legacy_path.clone(),
            legacy_read.clone(),
        );
        let migrated =
            migrate_legacy_named_drafts_with_reader(&current, &legacy, &mut migration_reader)
                .expect("malformed entry should remain within the migration budget");
        assert_eq!(migrated, 0);
        assert!(legacy_path.is_file());

        let mut save_reader = save_preflight_budget_reader(
            current.clone(),
            MAX_NAMED_DRAFT_BYTES,
            legacy_path,
            legacy_read,
        );
        let error = ensure_named_draft_save_fits_with_reader(
            &current,
            Some(&legacy),
            None,
            1,
            &mut save_reader,
        )
        .expect_err("retained malformed bytes must count against a new save");
        assert!(
            error.to_string().contains("would use 33554433 bytes"),
            "{error:#}"
        );
    }

    #[test]
    fn save_preflight_charges_oversized_legacy_bytes_retained_after_migration() {
        let root = tempfile::tempdir().expect("tempdir");
        let current = root.path().join("current");
        let legacy = root.path().join("legacy");
        fs::create_dir_all(&current).expect("current dir");
        fs::create_dir_all(&legacy).expect("legacy dir");
        let current_count = MAX_NAMED_DRAFT_TOTAL_BYTES / MAX_NAMED_DRAFT_BYTES - 2;
        create_named_draft_placeholders(&current, current_count);
        let legacy_path = legacy.join("retained-oversized.json");
        fs::write(&legacy_path, b"fixture").expect("write legacy draft placeholder");

        let legacy_read = SyntheticLegacyRead::Bytes(MAX_NAMED_DRAFT_BYTES + 1);
        let mut migration_reader = save_preflight_budget_reader(
            current.clone(),
            MAX_NAMED_DRAFT_BYTES,
            legacy_path.clone(),
            legacy_read.clone(),
        );
        let migrated =
            migrate_legacy_named_drafts_with_reader(&current, &legacy, &mut migration_reader)
                .expect("oversized entry should remain within the migration budget");
        assert_eq!(migrated, 0);
        assert!(legacy_path.is_file());

        let mut save_reader = save_preflight_budget_reader(
            current.clone(),
            MAX_NAMED_DRAFT_BYTES,
            legacy_path,
            legacy_read,
        );
        let error = ensure_named_draft_save_fits_with_reader(
            &current,
            Some(&legacy),
            None,
            MAX_NAMED_DRAFT_BYTES,
            &mut save_reader,
        )
        .expect_err("retained oversized bytes must count against a new save");
        assert!(
            error.to_string().contains("would use 33554433 bytes"),
            "{error:#}"
        );
    }

    #[test]
    fn save_preflight_does_not_charge_partial_legacy_read_errors() {
        let root = tempfile::tempdir().expect("tempdir");
        let current = root.path().join("current");
        let legacy = root.path().join("legacy");
        fs::create_dir_all(&current).expect("current dir");
        fs::create_dir_all(&legacy).expect("legacy dir");
        let current_count = MAX_NAMED_DRAFT_TOTAL_BYTES / MAX_NAMED_DRAFT_BYTES - 1;
        create_named_draft_placeholders(&current, current_count);
        let legacy_path = legacy.join("retained-partial-error.json");
        fs::write(&legacy_path, b"fixture").expect("write legacy draft placeholder");

        let migration_emitted = Rc::new(Cell::new(0));
        let mut migration_reader = save_preflight_budget_reader(
            current.clone(),
            MAX_NAMED_DRAFT_BYTES,
            legacy_path.clone(),
            SyntheticLegacyRead::PartialError {
                bytes: MAX_NAMED_DRAFT_BYTES,
                emitted: Rc::clone(&migration_emitted),
            },
        );
        let migrated =
            migrate_legacy_named_drafts_with_reader(&current, &legacy, &mut migration_reader)
                .expect("partial read error must not consume the migration budget");
        assert_eq!(migrated, 0);
        assert!(migration_emitted.get() > 0);
        assert!(legacy_path.is_file());

        let save_emitted = Rc::new(Cell::new(0));
        let mut save_reader = save_preflight_budget_reader(
            current.clone(),
            MAX_NAMED_DRAFT_BYTES,
            legacy_path,
            SyntheticLegacyRead::PartialError {
                bytes: MAX_NAMED_DRAFT_BYTES,
                emitted: Rc::clone(&save_emitted),
            },
        );
        ensure_named_draft_save_fits_with_reader(
            &current,
            Some(&legacy),
            None,
            MAX_NAMED_DRAFT_BYTES,
            &mut save_reader,
        )
        .expect("partial legacy read bytes must not consume the save budget");
        assert!(save_emitted.get() > 0);
    }

    #[test]
    fn physical_json_entry_count_is_rejected_before_entry_reads() {
        let root = tempfile::tempdir().expect("tempdir");
        for index in 0..=MAX_NAMED_DRAFTS {
            fs::create_dir(root.path().join(format!("unreadable-{index}.json")))
                .expect("create unreadable JSON entry");
        }

        let error = scan_named_drafts(root.path(), None)
            .expect_err("physical JSON count must be enforced before scanning entries");
        assert!(error.to_string().contains("257 JSON files"), "{error:#}");
    }

    #[test]
    fn duplicate_valid_drafts_are_deduplicated_while_bad_entries_are_reported() {
        let root = tempfile::tempdir().expect("tempdir");
        let current = root.path().join("current");
        let legacy = root.path().join("legacy");
        fs::create_dir_all(&current).expect("current dir");
        fs::create_dir_all(&legacy).expect("legacy dir");
        let bytes = serde_json::to_vec(&fields("same")).expect("serialize");
        fs::write(current.join("same.json"), &bytes).expect("current draft");
        fs::write(legacy.join("same.json"), &bytes).expect("legacy draft");
        fs::write(legacy.join("bad.json"), b"{").expect("malformed legacy draft");

        let loaded = load_named_drafts(&NamedDraftLoadRequest {
            generation: 1,
            current_dir: current,
            legacy_dir: Some(legacy),
            migrate_legacy: false,
            fixture_delay: Duration::ZERO,
        })
        .expect("load current and legacy drafts");
        assert_eq!(loaded.drafts.len(), 1);
        assert_eq!(loaded.drafts[0].fields.subject, "same");
        let warning = loaded.warning.expect("malformed-entry warning");
        assert!(warning.contains("rejected 1"), "{warning}");
        assert!(warning.contains("bad.json"), "{warning}");
    }

    #[test]
    fn unreadable_legacy_entry_does_not_block_safe_valid_migration() {
        let root = tempfile::tempdir().expect("tempdir");
        let current = root.path().join("current");
        let legacy = root.path().join("legacy");
        fs::create_dir_all(&current).expect("current dir");
        fs::create_dir_all(&legacy).expect("legacy dir");
        fs::write(
            current.join("current.json"),
            serde_json::to_vec(&fields("current")).expect("serialize current"),
        )
        .expect("write current");
        fs::write(
            legacy.join("legacy.json"),
            serde_json::to_vec(&fields("legacy")).expect("serialize legacy"),
        )
        .expect("write legacy");
        fs::create_dir(legacy.join("unreadable.json")).expect("unreadable legacy entry");

        let loaded = load_named_drafts(&NamedDraftLoadRequest {
            generation: 1,
            current_dir: current.clone(),
            legacy_dir: Some(legacy.clone()),
            migrate_legacy: true,
            fixture_delay: Duration::ZERO,
        })
        .expect("load drafts after migration failure");
        let subjects = loaded
            .drafts
            .iter()
            .map(|draft| draft.fields.subject.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(subjects, BTreeSet::from(["current", "legacy"]));
        assert_eq!(loaded.migrated, 1);
        assert!(current.join("legacy.json").is_file());
        assert!(!legacy.join("legacy.json").exists());
        assert!(legacy.join("unreadable.json").is_dir());
        let warning = loaded.warning.expect("rejected-entry warning");
        assert!(
            warning.contains("rejected 1") && warning.contains("unreadable.json"),
            "{warning}"
        );
    }

    #[test]
    fn malformed_legacy_entry_is_preserved_while_valid_drafts_migrate_and_load() {
        let root = tempfile::tempdir().expect("tempdir");
        let current = root.path().join("current");
        let legacy = root.path().join("legacy");
        fs::create_dir_all(&current).expect("current dir");
        fs::create_dir_all(&legacy).expect("legacy dir");
        fs::write(
            current.join("current.json"),
            serde_json::to_vec(&fields("current")).expect("serialize current"),
        )
        .expect("write current");
        fs::write(
            legacy.join("legacy.json"),
            serde_json::to_vec(&fields("legacy")).expect("serialize legacy"),
        )
        .expect("write legacy");
        let malformed_path = legacy.join("malformed.json");
        let malformed = b"{\"subject\":\"truncated";
        fs::write(&malformed_path, malformed).expect("write malformed legacy draft");

        let loaded = load_named_drafts(&NamedDraftLoadRequest {
            generation: 1,
            current_dir: current.clone(),
            legacy_dir: Some(legacy.clone()),
            migrate_legacy: true,
            fixture_delay: Duration::ZERO,
        })
        .expect("load drafts while preserving malformed legacy entry");
        let subjects = loaded
            .drafts
            .iter()
            .map(|draft| draft.fields.subject.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(subjects, BTreeSet::from(["current", "legacy"]));
        assert_eq!(loaded.migrated, 1);
        assert!(current.join("legacy.json").is_file());
        assert!(!current.join("malformed.json").exists());
        assert!(!legacy.join("legacy.json").exists());
        assert_eq!(
            fs::read(&malformed_path).expect("read preserved malformed draft"),
            malformed
        );
        let warning = loaded.warning.expect("malformed-entry warning");
        assert!(
            warning.contains("rejected 1") && warning.contains("malformed.json"),
            "{warning}"
        );
    }

    #[test]
    fn coordinator_rejects_stale_completion() {
        let mut coordinator = DraftIoCoordinator::default();
        let old = coordinator.begin(false).expect("first refresh");
        let current = coordinator.begin(false).expect("replacement refresh");
        assert!(!coordinator.accepts(old));
        assert!(coordinator.accepts(current));
        assert!(!coordinator.finish(old));
        assert!(coordinator.finish(current));
        assert_eq!(coordinator.completed_generation(), Some(current));
    }

    #[test]
    fn coordinator_keeps_migration_exclusive_until_exact_completion() {
        let mut coordinator = DraftIoCoordinator::default();
        let migration = coordinator.begin(true).expect("start migration");
        assert!(coordinator.migration_in_progress());
        assert_eq!(coordinator.begin(false), None);
        assert_eq!(coordinator.begin(true), None);
        assert_eq!(coordinator.active_generation(), Some(migration));

        assert!(!coordinator.finish(migration.saturating_add(1)));
        assert!(coordinator.migration_in_progress());
        assert_eq!(coordinator.active_generation(), Some(migration));

        assert!(coordinator.finish(migration));
        assert!(!coordinator.migration_in_progress());
        let refresh = coordinator.begin(false).expect("refresh after migration");
        assert!(refresh > migration);
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
        let load_error = load_named_drafts(&NamedDraftLoadRequest {
            generation: 1,
            current_dir: current.clone(),
            legacy_dir: Some(legacy.clone()),
            migrate_legacy: true,
            fixture_delay: Duration::ZERO,
        })
        .expect_err("store-wide migration policy failure must remain fatal");
        assert!(
            load_error.to_string().contains("would contain 257"),
            "{load_error:#}"
        );
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
                .drafts
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
                .drafts
                .len(),
            full_draft_count
        );
    }
}
