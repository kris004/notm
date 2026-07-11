use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttachmentRef {
    pub filename: String,
    pub content_type: String,
    pub size: usize,
}

/// Save attachment bytes without replacing a file that is already present.
///
/// The first save uses the sanitized attachment filename. Later saves add a
/// numeric suffix before the final extension. `create_new` atomically reserves
/// each candidate, so concurrent saves cannot select the same path.
pub fn save_attachment_without_overwrite(
    target_dir: &Path,
    filename: &str,
    bytes: &[u8],
) -> io::Result<PathBuf> {
    fs::create_dir_all(target_dir)?;
    let filename = safe_filename(filename);
    let mut collision_index = 0_u64;

    loop {
        let candidate = numbered_filename(&filename, collision_index);
        let path = target_dir.join(candidate);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                file.write_all(bytes)?;
                return Ok(path);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                collision_index = collision_index.checked_add(1).ok_or_else(|| {
                    io::Error::other("attachment filename collision counter overflowed")
                })?;
            }
            Err(error) => return Err(error),
        }
    }
}

fn safe_filename(filename: &str) -> String {
    let cleaned = filename
        .chars()
        .map(|character| match character {
            '/' | '\\' | '\0' => '_',
            _ => character,
        })
        .collect::<String>();
    if cleaned.trim().is_empty() || matches!(cleaned.as_str(), "." | "..") {
        "attachment.bin".to_string()
    } else {
        cleaned
    }
}

fn numbered_filename(filename: &str, collision_index: u64) -> String {
    if collision_index == 0 {
        return filename.to_string();
    }

    match filename.rfind('.') {
        Some(dot_index) if dot_index > 0 && dot_index + 1 < filename.len() => format!(
            "{} ({collision_index}){}",
            &filename[..dot_index],
            &filename[dot_index..]
        ),
        _ => format!("{filename} ({collision_index})"),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        fs,
        sync::{Arc, Barrier},
        thread,
    };

    use super::*;

    #[test]
    fn new_attachment_uses_requested_name() {
        let directory = tempfile::tempdir().expect("temporary directory");

        let path = save_attachment_without_overwrite(directory.path(), "note.txt", b"attachment")
            .expect("save attachment");

        assert_eq!(path, directory.path().join("note.txt"));
        assert_eq!(fs::read(path).expect("read attachment"), b"attachment");
    }

    #[test]
    fn existing_attachment_is_kept_and_numbered_copy_preserves_extension() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let original = directory.path().join("archive.tar.gz");
        fs::write(&original, b"keep original").expect("write original");

        let first = save_attachment_without_overwrite(
            directory.path(),
            "archive.tar.gz",
            b"first attachment",
        )
        .expect("save first numbered attachment");
        let second = save_attachment_without_overwrite(
            directory.path(),
            "archive.tar.gz",
            b"second attachment",
        )
        .expect("save second numbered attachment");

        assert_eq!(first, directory.path().join("archive.tar (1).gz"));
        assert_eq!(second, directory.path().join("archive.tar (2).gz"));
        assert_eq!(fs::read(original).expect("read original"), b"keep original");
        assert_eq!(fs::read(first).expect("read first"), b"first attachment");
        assert_eq!(fs::read(second).expect("read second"), b"second attachment");
    }

    #[test]
    fn concurrent_attachment_saves_reserve_distinct_paths() {
        const SAVE_COUNT: usize = 12;

        let directory = tempfile::tempdir().expect("temporary directory");
        let target_dir = Arc::new(directory.path().to_path_buf());
        let barrier = Arc::new(Barrier::new(SAVE_COUNT));
        let handles = (0..SAVE_COUNT)
            .map(|index| {
                let target_dir = Arc::clone(&target_dir);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    let bytes = format!("attachment {index}").into_bytes();
                    barrier.wait();
                    let path = save_attachment_without_overwrite(&target_dir, "note.txt", &bytes)
                        .expect("save concurrent attachment");
                    (path, bytes)
                })
            })
            .collect::<Vec<_>>();

        let saved = handles
            .into_iter()
            .map(|handle| handle.join().expect("attachment save thread"))
            .collect::<Vec<_>>();
        let paths = saved
            .iter()
            .map(|(path, _)| path.clone())
            .collect::<BTreeSet<_>>();

        assert_eq!(paths.len(), SAVE_COUNT);
        for (path, expected) in saved {
            assert_eq!(fs::read(path).expect("read saved attachment"), expected);
        }
    }

    #[test]
    fn attachment_filename_cannot_escape_target_directory() {
        let root = tempfile::tempdir().expect("temporary directory");
        let target_dir = root.path().join("downloads");

        let path =
            save_attachment_without_overwrite(&target_dir, "../../outside.txt", b"attachment")
                .expect("save sanitized attachment");

        assert_eq!(path, target_dir.join(".._.._outside.txt"));
        assert_eq!(path.parent(), Some(target_dir.as_path()));
        assert_eq!(fs::read(path).expect("read attachment"), b"attachment");
        assert!(!root.path().join("outside.txt").exists());
    }
}
