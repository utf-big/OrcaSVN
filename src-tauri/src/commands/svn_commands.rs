//! The SVN command boundary: one thin wrapper per working copy operation.
use crate::models::{
    CommandError, CommandResult, DiffResult, SvnAuthUser, SvnInfo, SvnLogEntry, SvnStatus,
};
use crate::svn;
use crate::svn::executor::StreamKind;
use serde::Serialize;
use std::path::Path;
use tauri::{Emitter, Manager};

/// One line of checkout progress, streamed to the checkout page.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckoutProgress {
    pub kind: String,
    pub line: String,
}

/// Continues a cancelled or interrupted checkout of an existing working copy.
#[tauri::command]
pub(crate) async fn svn_resume_checkout(
    app: tauri::AppHandle,
    path: String,
) -> Result<svn::CheckoutReport, CommandError> {
    let target = path.clone();
    let handle = app.clone();
    let sink = move |kind: StreamKind, line: String| {
        let _ = handle.emit(
            "checkout-progress",
            CheckoutProgress {
                kind: kind.as_str().to_string(),
                line,
            },
        );
    };
    svn::gate::cancellable(Path::new(&path), async move {
        svn::resume_checkout(&target, sink).await
    })
    .await
    .map_err(CommandError::from)
}

/// Fetches everything the server can still serve, skipping unreadable subtrees.
#[tauri::command]
pub(crate) async fn svn_checkout_salvage(
    app: tauri::AppHandle,
    url: String,
    path: String,
) -> Result<svn::SalvageReport, CommandError> {
    let target = path.clone();
    let handle = app.clone();
    let sink = move |kind: StreamKind, line: String| {
        let _ = handle.emit(
            "checkout-progress",
            CheckoutProgress {
                kind: kind.as_str().to_string(),
                line,
            },
        );
    };
    svn::gate::cancellable(Path::new(&path), async move {
        svn::checkout_salvage(&url, &target, sink).await
    })
    .await
    .map_err(CommandError::from)
}

/// Checks out while streaming every SVN line to the interface, and retries a
/// truncated XML response instead of failing the whole checkout.
#[tauri::command]
pub(crate) async fn svn_checkout_stream(
    app: tauri::AppHandle,
    url: String,
    path: String,
    revision: Option<u64>,
) -> Result<svn::CheckoutReport, CommandError> {
    let target = path.clone();
    let handle = app.clone();
    let sink = move |kind: StreamKind, line: String| {
        let _ = handle.emit(
            "checkout-progress",
            CheckoutProgress {
                kind: kind.as_str().to_string(),
                line,
            },
        );
    };
    let salvage_sink = sink.clone();
    svn::gate::cancellable(Path::new(&path), async move {
        match svn::checkout_streaming(&url, &target, revision, sink).await {
            Ok(report) => Ok(report),
            // The server cannot read part of its own repository. Fetching the
            // rest is the only client-side remedy, so take it automatically
            // instead of leaving the user with a dead end.
            Err(error)
                if svn::is_server_storage_failure(&error.to_string())
                    || svn::is_reserved_name_error(&error.to_string()) =>
            {
                salvage_sink(
                    StreamKind::Status,
                    "检出无法完整完成，自动改为跳过失败项继续检出".to_string(),
                );
                let salvage = svn::checkout_salvage(&url, &target, salvage_sink).await?;
                Ok(svn::CheckoutReport {
                    output: salvage.output,
                    attempts: 0,
                    revision: None,
                    salvaged: true,
                    failed: salvage.failed,
                    missing_files: salvage.missing_files,
                })
            }
            Err(error) => Err(error),
        }
    })
    .await
    .map_err(CommandError::from)
}

#[tauri::command]
pub(crate) async fn svn_update(
    path: String,
    revision: Option<u64>,
) -> Result<CommandResult, CommandError> {
    let target = path.clone();
    svn::gate::cancellable(Path::new(&path), async move {
        svn::update(&target, revision).await
    })
    .await
    .map(|output| CommandResult {
        success: true,
        output,
        error: None,
    })
    .map_err(CommandError::from)
}

/// Removes a repository-side duplicate spelling while keeping the local file.
#[tauri::command]
pub(crate) async fn svn_delete_duplicates(
    path: String,
    files: Vec<String>,
) -> Result<CommandResult, CommandError> {
    svn::delete_duplicate_names(&path, &files)
        .await
        .map(|output| CommandResult {
            success: true,
            output,
            error: None,
        })
        .map_err(CommandError::from)
}

/// Renames files whose on-disk Unicode form differs from the repository one.
#[tauri::command]
pub(crate) async fn svn_repair_names(
    path: String,
    files: Vec<String>,
) -> Result<usize, CommandError> {
    svn::repair_name_mismatches(&path, &files)
        .await
        .map_err(CommandError::from)
}

/// Aborts the running checkout or update for this working copy.
#[tauri::command]
pub(crate) async fn svn_cancel(path: String) -> Result<bool, CommandError> {
    Ok(svn::gate::cancel(Path::new(&path)))
}

#[tauri::command]
pub(crate) async fn svn_commit(
    path: String,
    message: String,
    files: Option<Vec<String>>,
) -> Result<CommandResult, CommandError> {
    svn::commit(&path, &message, files.as_deref())
        .await
        .map(|output| CommandResult {
            success: true,
            output,
            error: None,
        })
        .map_err(CommandError::from)
}

#[tauri::command]
pub(crate) async fn svn_status(
    app: tauri::AppHandle,
    path: String,
    force: Option<bool>,
) -> Result<Vec<SvnStatus>, CommandError> {
    svn::cached_status(
        &path,
        force.unwrap_or(false),
        app.path()
            .app_cache_dir()
            .ok()
            .map(|path| path.join("workspace-status")),
    )
    .await
    .map_err(CommandError::from)
}

#[tauri::command]
pub(crate) async fn svn_log(
    path: String,
    limit: Option<u32>,
    start_rev: Option<u64>,
    end_rev: Option<u64>,
    keyword: Option<String>,
    author: Option<String>,
    date_from: Option<String>,
    date_to: Option<String>,
) -> Result<Vec<SvnLogEntry>, CommandError> {
    svn::log(
        &path,
        limit,
        start_rev,
        end_rev,
        keyword.as_deref(),
        author.as_deref(),
        date_from.as_deref(),
        date_to.as_deref(),
    )
    .await
    .map_err(CommandError::from)
}

#[tauri::command]
pub(crate) async fn svn_current_user(path: String) -> Result<Option<SvnAuthUser>, CommandError> {
    svn::current_user(&path).await.map_err(CommandError::from)
}

#[tauri::command]
pub(crate) async fn svn_info(path: String) -> Result<SvnInfo, CommandError> {
    svn::info(&path).await.map_err(CommandError::from)
}

#[tauri::command]
pub(crate) async fn svn_local_revision(path: String) -> Result<u64, CommandError> {
    svn::local_revision(&path).await.map_err(CommandError::from)
}

#[tauri::command]
pub(crate) async fn svn_remote_info(path: String) -> Result<SvnInfo, CommandError> {
    svn::remote_info(&path).await.map_err(CommandError::from)
}

#[tauri::command]
pub(crate) async fn svn_diff(
    workspace_path: String,
    file: String,
    old_rev: Option<u64>,
    new_rev: Option<u64>,
) -> Result<DiffResult, CommandError> {
    svn::diff(&workspace_path, &file, old_rev, new_rev)
        .await
        .map_err(CommandError::from)
}

#[tauri::command]
pub(crate) async fn svn_apply_patch(
    workspace_path: String,
    patch: String,
    reverse: bool,
) -> Result<CommandResult, CommandError> {
    svn::apply_patch(&workspace_path, &patch, reverse)
        .await
        .map(|output| CommandResult {
            success: true,
            output,
            error: None,
        })
        .map_err(CommandError::from)
}

#[tauri::command]
pub(crate) async fn svn_blame(
    workspace_path: String,
    file: String,
) -> Result<Vec<svn::BlameLine>, CommandError> {
    svn::blame(&workspace_path, &file)
        .await
        .map_err(CommandError::from)
}

#[tauri::command]
pub(crate) async fn svn_add(
    path: String,
    files: Vec<String>,
) -> Result<CommandResult, CommandError> {
    svn::add(&path, &files)
        .await
        .map(|output| CommandResult {
            success: true,
            output,
            error: None,
        })
        .map_err(CommandError::from)
}

#[tauri::command]
pub(crate) async fn svn_delete(
    path: String,
    files: Vec<String>,
) -> Result<CommandResult, CommandError> {
    svn::delete(&path, &files)
        .await
        .map(|output| CommandResult {
            success: true,
            output,
            error: None,
        })
        .map_err(CommandError::from)
}

#[tauri::command]
pub(crate) async fn svn_revert(
    path: String,
    files: Vec<String>,
) -> Result<CommandResult, CommandError> {
    svn::revert(&path, &files)
        .await
        .map(|output| CommandResult {
            success: true,
            output,
            error: None,
        })
        .map_err(CommandError::from)
}

#[tauri::command]
pub(crate) async fn svn_resolve(
    path: String,
    files: Vec<String>,
    strategy: String,
) -> Result<CommandResult, CommandError> {
    svn::resolve(&path, &files, &strategy)
        .await
        .map(|output| CommandResult {
            success: true,
            output,
            error: None,
        })
        .map_err(CommandError::from)
}

#[tauri::command]
pub(crate) async fn svn_cleanup(path: String) -> Result<CommandResult, CommandError> {
    svn::cleanup(&path)
        .await
        .map(|output| CommandResult {
            success: true,
            output,
            error: None,
        })
        .map_err(CommandError::from)
}

#[tauri::command]
pub(crate) async fn svn_switch(path: String, url: String) -> Result<CommandResult, CommandError> {
    svn::switch_cmd(&path, &url)
        .await
        .map(|output| CommandResult {
            success: true,
            output,
            error: None,
        })
        .map_err(CommandError::from)
}

#[tauri::command]
pub(crate) async fn svn_merge(
    path: String,
    source: String,
    rev_start: u64,
    rev_end: u64,
) -> Result<CommandResult, CommandError> {
    svn::merge(&path, &source, rev_start, rev_end)
        .await
        .map(|output| CommandResult {
            success: true,
            output,
            error: None,
        })
        .map_err(CommandError::from)
}
