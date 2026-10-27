//! Durable stash storage for the working copy.
//!
//! Stashes used to live in WebView local storage: clearing site data destroyed
//! the only copy, and the base64 budget limited how many unversioned files a
//! stash could hold. Records now live under the app data directory, one JSON
//! file per stash, keyed by the working copy they belong to.

use crate::models::{CommandError, StashRecord};
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use tauri::Manager;

/// Bounds one record in memory while it is serialized; stashes are written to
/// disk instead of shared memory, so this is far above the old 2 MB limit.
const MAX_STASH_BYTES: usize = 64 * 1024 * 1024;

fn io_error(error: std::io::Error) -> CommandError {
    CommandError::new("io_error", format!("无法写入贮藏数据：{error}"))
}

/// Stable, filesystem-safe key for a working copy path.
fn workspace_key(workspace: &str) -> String {
    let canonical = fs::canonicalize(workspace).unwrap_or_else(|_| PathBuf::from(workspace));
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in canonical.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn stash_directory(app: &tauri::AppHandle, workspace: &str) -> Result<PathBuf, CommandError> {
    let base = app.path().app_data_dir().map_err(|error| {
        CommandError::new(
            "storage_unavailable",
            format!("无法访问应用数据目录：{error}"),
        )
    })?;
    Ok(base.join("stashes").join(workspace_key(workspace)))
}

/// Ids come from the interface (`crypto.randomUUID`) and name a file, so only
/// that alphabet may reach the filesystem.
fn safe_id(id: &str) -> Result<&str, CommandError> {
    let valid = !id.is_empty()
        && id.len() <= 128
        && id.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '-' || character == '_'
        });
    if valid {
        Ok(id)
    } else {
        Err(CommandError::new("invalid_stash", "贮藏记录 ID 无效"))
    }
}

fn record_path(directory: &Path, id: &str) -> Result<PathBuf, CommandError> {
    Ok(directory.join(format!("{}.json", safe_id(id)?)))
}

/// Writes through a temporary file so an interrupted save cannot leave a
/// half-written record that replaces a good one.
fn write_stash(directory: &Path, stash: &StashRecord) -> Result<(), CommandError> {
    let path = record_path(directory, &stash.id)?;
    let bytes = serde_json::to_vec(stash)
        .map_err(|error| CommandError::new("invalid_stash", error.to_string()))?;
    if bytes.len() > MAX_STASH_BYTES {
        return Err(CommandError::new(
            "stash_too_large",
            "贮藏内容超过 64 MB 上限",
        ));
    }
    fs::create_dir_all(directory).map_err(io_error)?;
    let temporary = directory.join(format!(".{}.tmp", stash.id));
    let mut file = fs::File::create(&temporary).map_err(io_error)?;
    file.write_all(&bytes).map_err(io_error)?;
    file.sync_all().map_err(io_error)?;
    fs::rename(&temporary, &path).map_err(io_error)?;
    Ok(())
}

fn read_stashes(directory: &Path) -> Result<Vec<StashRecord>, CommandError> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(io_error(error)),
    };
    let mut records = Vec::new();
    for entry in entries {
        let path = entry.map_err(io_error)?.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        // Keep unreadable files on disk for recovery instead of deleting them.
        match fs::read(&path)
            .map_err(|error| error.to_string())
            .and_then(|bytes| {
                serde_json::from_slice::<StashRecord>(&bytes).map_err(|error| error.to_string())
            }) {
            Ok(record) => records.push(record),
            Err(error) => {
                tracing::warn!(%error, file = %path.display(), "skipping unreadable stash record");
            }
        }
    }
    records.sort_by(|left, right| right.created_at.cmp(&left.created_at));
    Ok(records)
}

#[tauri::command]
pub(crate) async fn save_stash(
    app: tauri::AppHandle,
    stash: StashRecord,
) -> Result<(), CommandError> {
    let directory = stash_directory(&app, &stash.workspace_path)?;
    tokio::task::spawn_blocking(move || write_stash(&directory, &stash))
        .await
        .map_err(|error| CommandError::new("internal", error.to_string()))?
}

#[tauri::command]
pub(crate) async fn list_stashes(
    app: tauri::AppHandle,
    workspace_path: String,
) -> Result<Vec<StashRecord>, CommandError> {
    let directory = stash_directory(&app, &workspace_path)?;
    tokio::task::spawn_blocking(move || read_stashes(&directory))
        .await
        .map_err(|error| CommandError::new("internal", error.to_string()))?
}

#[tauri::command]
pub(crate) async fn delete_stash(
    app: tauri::AppHandle,
    workspace_path: String,
    id: String,
) -> Result<(), CommandError> {
    let directory = stash_directory(&app, &workspace_path)?;
    tokio::task::spawn_blocking(move || {
        let path = record_path(&directory, &id)?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(io_error(error)),
        }
    })
    .await
    .map_err(|error| CommandError::new("internal", error.to_string()))?
}

/// Moves records written by older versions out of WebView local storage.
/// Existing files are kept, so a repeated import is harmless.
#[tauri::command]
pub(crate) async fn import_legacy_stashes(
    app: tauri::AppHandle,
    workspace_path: String,
    stashes: Vec<StashRecord>,
) -> Result<usize, CommandError> {
    let directory = stash_directory(&app, &workspace_path)?;
    tokio::task::spawn_blocking(move || {
        let mut imported = 0;
        for stash in &stashes {
            let path = record_path(&directory, &stash.id)?;
            if path.exists() {
                continue;
            }
            write_stash(&directory, stash)?;
            imported += 1;
        }
        Ok(imported)
    })
    .await
    .map_err(|error| CommandError::new("internal", error.to_string()))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::StashUnversionedEntry;

    fn record(id: &str, created_at: &str) -> StashRecord {
        StashRecord {
            id: id.to_string(),
            name: "wip".to_string(),
            workspace_path: "/wc".to_string(),
            created_at: created_at.to_string(),
            patch: "diff".to_string(),
            files: vec!["a.txt".to_string()],
            hunk_count: 1,
            unversioned_files: Some(vec![StashUnversionedEntry {
                path: "new.txt".to_string(),
                kind: "file".to_string(),
                content_base64: Some("aGk=".to_string()),
            }]),
        }
    }

    fn directory() -> PathBuf {
        std::env::temp_dir().join(format!(
            "orcasvn-stash-store-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn records_round_trip_and_are_listed_newest_first() {
        let root = directory();
        write_stash(&root, &record("older", "2026-01-01T00:00:00Z")).unwrap();
        write_stash(&root, &record("newer", "2026-02-01T00:00:00Z")).unwrap();
        let listed = read_stashes(&root).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, "newer");
        assert_eq!(
            listed[1].unversioned_files.as_ref().unwrap()[0].path,
            "new.txt"
        );
        // No temporary files survive a successful save.
        let leftovers: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().starts_with('.'))
            .collect();
        assert!(leftovers.is_empty());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_missing_directory_lists_as_empty_and_unknown_ids_delete_quietly() {
        let root = directory();
        assert!(read_stashes(&root).unwrap().is_empty());
        let path = record_path(&root, "missing").unwrap();
        assert!(
            matches!(fs::remove_file(&path), Err(error) if error.kind() == ErrorKind::NotFound)
        );
    }

    #[test]
    fn ids_that_would_escape_the_directory_are_rejected() {
        for id in ["", "../escape", "a/b", "a\\b", "a.json", &"x".repeat(129)] {
            assert!(safe_id(id).is_err(), "{id}");
        }
        assert_eq!(
            safe_id("5f2b4c1e-0a0e-4b6c-8f3d-2f4c5d6e7a8b")
                .unwrap()
                .len(),
            36
        );
    }

    #[test]
    fn unreadable_records_are_skipped_but_kept_on_disk() {
        let root = directory();
        write_stash(&root, &record("good", "2026-01-01T00:00:00Z")).unwrap();
        fs::write(root.join("broken.json"), b"{not json").unwrap();
        let listed = read_stashes(&root).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "good");
        assert!(root.join("broken.json").exists());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_workspace_key_is_stable_and_distinct_per_path() {
        let first = workspace_key("/tmp/a");
        assert_eq!(first, workspace_key("/tmp/a"));
        assert_ne!(first, workspace_key("/tmp/b"));
        assert_eq!(first.len(), 16);
    }
}
