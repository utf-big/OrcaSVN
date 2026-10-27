//! Local storage of unversioned files for the stash feature.
use crate::commands::workspace::resolve_workspace_child;
use crate::models::{CommandError, CommandResult, StashUnversionedEntry};
use crate::svn;
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use std::fs;
use std::path::Path;

#[tauri::command]
pub(crate) async fn delete_unversioned(
    path: String,
    files: Vec<String>,
) -> Result<CommandResult, CommandError> {
    let workspace = fs::canonicalize(&path)
        .map_err(|e| CommandError::new("workspace_invalid", format!("无法访问工作副本：{e}")))?;
    let mut removed = Vec::new();

    for file in files {
        let target = resolve_workspace_child(&workspace, &file)?;
        if !target.exists() {
            return Err(CommandError::new(
                "file_missing",
                format!("文件不存在：{file}"),
            ));
        }

        let canonical_target = fs::canonicalize(&target).map_err(|e| {
            CommandError::new("file_missing", format!("无法解析文件路径“{file}”：{e}"))
        })?;
        if !canonical_target.starts_with(&workspace) {
            return Err(CommandError::new(
                "path_outside_workspace",
                format!("拒绝删除工作副本之外的目标：{file}"),
            ));
        }

        if canonical_target.is_dir() {
            fs::remove_dir_all(&canonical_target).map_err(|e| {
                CommandError::new("operation_failed", format!("无法删除目录“{file}”：{e}"))
            })?;
        } else {
            fs::remove_file(&canonical_target).map_err(|e| {
                CommandError::new("operation_failed", format!("无法删除文件“{file}”：{e}"))
            })?;
        }
        removed.push(file);
    }

    Ok(CommandResult {
        success: true,
        output: removed.join("\n"),
        error: None,
    })
}

// Records are written to the app data directory, so the ceiling only guards
// how much file content is held in memory and sent over IPC in one go.
const MAX_UNVERSIONED_STASH_BYTES: u64 = 64 * 1024 * 1024;

fn collect_unversioned_entry(
    workspace: &Path,
    target: &Path,
    entries: &mut Vec<StashUnversionedEntry>,
    total_bytes: &mut u64,
) -> Result<(), CommandError> {
    let metadata =
        fs::symlink_metadata(target).map_err(|e| CommandError::new("io_error", e.to_string()))?;
    if metadata.file_type().is_symlink() {
        return Err(CommandError::new(
            "stash_unsupported",
            format!("不支持符号链接：{}", target.display()),
        ));
    }
    let relative = target
        .strip_prefix(workspace)
        .map_err(|e| CommandError::new("io_error", e.to_string()))?;
    let path = relative.to_string_lossy().replace('\\', "/");
    if metadata.is_dir() {
        entries.push(StashUnversionedEntry {
            path,
            kind: "directory".to_string(),
            content_base64: None,
        });
        let mut children = fs::read_dir(target)
            .map_err(|e| CommandError::new("io_error", e.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| CommandError::new("io_error", e.to_string()))?;
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            collect_unversioned_entry(workspace, &child.path(), entries, total_bytes)?;
        }
    } else {
        *total_bytes = total_bytes.saturating_add(metadata.len());
        if *total_bytes > MAX_UNVERSIONED_STASH_BYTES {
            return Err(CommandError::new(
                "stash_too_large",
                "未跟踪文件超过贮藏大小上限（64 MB）",
            ));
        }
        let content = fs::read(target).map_err(|e| CommandError::new("io_error", e.to_string()))?;
        entries.push(StashUnversionedEntry {
            path,
            kind: "file".to_string(),
            content_base64: Some(BASE64_STANDARD.encode(content)),
        });
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn read_unversioned_files(
    path: String,
    files: Vec<String>,
) -> Result<Vec<StashUnversionedEntry>, CommandError> {
    let workspace = fs::canonicalize(&path)
        .map_err(|e| CommandError::new("workspace_invalid", format!("无法访问工作副本：{e}")))?;
    let statuses = svn::status(&path).await.map_err(CommandError::from)?;
    let unversioned: std::collections::HashSet<String> = statuses
        .into_iter()
        .filter(|status| status.status_code == "unversioned")
        .map(|status| status.path.replace('\\', "/"))
        .collect();
    let mut entries = Vec::new();
    let mut total_bytes = 0;
    for file in files {
        let normalized = file.replace('\\', "/");
        if !unversioned.contains(&normalized) {
            return Err(CommandError::new(
                "invalid_stash_target",
                format!("目标不是未跟踪文件：{file}"),
            ));
        }
        let target = resolve_workspace_child(&workspace, &file)?;
        let canonical =
            fs::canonicalize(&target).map_err(|e| CommandError::new("io_error", e.to_string()))?;
        if !canonical.starts_with(&workspace) {
            return Err(CommandError::new(
                "path_outside_workspace",
                format!("拒绝读取工作副本之外的目标：{file}"),
            ));
        }
        collect_unversioned_entry(&workspace, &canonical, &mut entries, &mut total_bytes)?;
    }
    Ok(entries)
}

fn validate_restore_target(workspace: &Path, target: &Path) -> Result<(), CommandError> {
    let mut existing_parent = target
        .parent()
        .ok_or_else(|| CommandError::new("workspace_invalid", "无效的恢复路径"))?;
    while !existing_parent.exists() {
        existing_parent = existing_parent
            .parent()
            .ok_or_else(|| CommandError::new("workspace_invalid", "无效的恢复路径"))?;
    }
    let canonical_parent = fs::canonicalize(existing_parent)
        .map_err(|e| CommandError::new("io_error", e.to_string()))?;
    if !canonical_parent.starts_with(workspace) {
        return Err(CommandError::new(
            "path_outside_workspace",
            format!("拒绝恢复到工作副本之外的目标：{}", target.display()),
        ));
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn restore_unversioned_files(
    path: String,
    files: Vec<StashUnversionedEntry>,
    dry_run: bool,
) -> Result<(), CommandError> {
    let workspace = fs::canonicalize(&path)
        .map_err(|e| CommandError::new("workspace_invalid", format!("无法访问工作副本：{e}")))?;
    if files.is_empty() {
        return Ok(());
    }

    let mut resolved = Vec::with_capacity(files.len());
    for entry in &files {
        if entry.kind != "file" && entry.kind != "directory" {
            return Err(CommandError::new(
                "invalid_stash",
                format!("贮藏记录的条目类型无效：{}", entry.kind),
            ));
        }
        if entry.kind == "file" && entry.content_base64.is_none() {
            return Err(CommandError::new(
                "invalid_stash",
                format!("贮藏记录缺少文件内容：{}", entry.path),
            ));
        }
        let target = resolve_workspace_child(&workspace, &entry.path)?;
        validate_restore_target(&workspace, &target)?;
        if target.exists() {
            return Err(CommandError::new(
                "target_exists",
                format!("目标已存在，无法恢复：{}", entry.path),
            ));
        }
        resolved.push((entry, target));
    }
    if dry_run {
        return Ok(());
    }

    resolved.sort_by_key(|(_, target)| target.components().count());
    for (entry, target) in resolved {
        if entry.kind == "directory" {
            fs::create_dir_all(&target).map_err(|e| {
                CommandError::new(
                    "operation_failed",
                    format!("无法恢复目录“{}”：{e}", entry.path),
                )
            })?;
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| CommandError::new("io_error", e.to_string()))?;
            }
            let encoded = entry.content_base64.as_deref().unwrap_or_default();
            let content = BASE64_STANDARD.decode(encoded).map_err(|e| {
                CommandError::new(
                    "invalid_stash",
                    format!("贮藏内容无效“{}”：{e}", entry.path),
                )
            })?;
            fs::write(&target, content).map_err(|e| {
                CommandError::new(
                    "operation_failed",
                    format!("无法恢复文件“{}”：{e}", entry.path),
                )
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod unversioned_stash_tests {
    use super::{collect_unversioned_entry, restore_unversioned_files};
    use std::fs;

    #[test]
    fn captures_and_restores_binary_files_and_empty_directories() {
        let root = std::env::temp_dir().join(format!(
            "orcasvn-unversioned-stash-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = root.join("new-dir");
        fs::create_dir_all(source.join("empty")).unwrap();
        fs::write(source.join("binary.dat"), [0, 1, 127, 255]).unwrap();

        let workspace = fs::canonicalize(&root).unwrap();
        let canonical_source = fs::canonicalize(&source).unwrap();
        let mut entries = Vec::new();
        let mut total_bytes = 0;
        collect_unversioned_entry(
            &workspace,
            &canonical_source,
            &mut entries,
            &mut total_bytes,
        )
        .unwrap();
        assert_eq!(total_bytes, 4);
        assert!(entries.iter().any(|entry| entry.path == "new-dir/empty"));

        fs::remove_dir_all(&source).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        runtime
            .block_on(restore_unversioned_files(
                root.to_string_lossy().into_owned(),
                entries.clone(),
                true,
            ))
            .unwrap();
        assert!(!source.exists());
        runtime
            .block_on(restore_unversioned_files(
                root.to_string_lossy().into_owned(),
                entries.clone(),
                false,
            ))
            .unwrap();
        assert_eq!(
            fs::read(source.join("binary.dat")).unwrap(),
            [0, 1, 127, 255]
        );
        assert!(source.join("empty").is_dir());
        assert!(runtime
            .block_on(restore_unversioned_files(
                root.to_string_lossy().into_owned(),
                entries,
                true,
            ))
            .is_err());
        fs::remove_dir_all(&root).unwrap();
    }
}
