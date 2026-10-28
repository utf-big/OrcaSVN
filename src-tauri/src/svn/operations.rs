use crate::models::{DiffResult, SvnAuthUser, SvnInfo, SvnLogEntry, SvnStatus};
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::executor::{
    execute_svn, execute_svn_network, execute_svn_streaming, StreamKind, SvnError,
    SVN_NETWORK_TIMEOUT, SVN_TIMEOUT,
};
use super::gate;
use super::parser::{parse_blame_text, parse_info_xml, parse_log_xml, parse_status_xml};
use unicode_normalization::UnicodeNormalization;

fn append_targets(args: &mut Vec<String>, targets: &[String]) {
    if targets.is_empty() {
        return;
    }
    args.push("--".to_string());
    args.extend(targets.iter().cloned());
}

fn normalize_diff_target(file: &str) -> String {
    if file.contains("://") || !file.contains('@') || file.ends_with('@') {
        file.to_string()
    } else {
        format!("{}@", file)
    }
}

fn normalize_svn_path(path: &str) -> String {
    path.replace('\\', "/")
}

fn normalize_workspace_diff_file(workspace: &str, file: &str) -> Result<String, SvnError> {
    if file.contains("://") {
        return Ok(file.to_string());
    }

    let file_path = Path::new(file);
    if !file_path.is_absolute() {
        return Ok(normalize_svn_path(file));
    }

    let workspace_path = Path::new(workspace)
        .canonicalize()
        .map_err(|e| SvnError::CommandFailed(format!("无法访问工作副本目录：{}", e)))?;
    let canonical_file = file_path
        .canonicalize()
        .map_err(|e| SvnError::CommandFailed(format!("无法访问文件：{}", e)))?;

    if !canonical_file.starts_with(&workspace_path) {
        return Err(SvnError::InvalidArguments(
            "文件必须位于当前工作副本内".to_string(),
        ));
    }

    let relative = canonical_file
        .strip_prefix(&workspace_path)
        .map_err(|e| SvnError::CommandFailed(format!("无法解析工作副本相对路径：{}", e)))?;

    Ok(normalize_svn_path(&relative.to_string_lossy()))
}

fn build_diff_args(
    file: &str,
    old_rev: Option<u64>,
    new_rev: Option<u64>,
) -> Result<Vec<String>, SvnError> {
    let mut args = vec!["diff".to_string()];

    match (old_rev, new_rev) {
        (Some(old), Some(new)) => {
            args.push("-r".to_string());
            args.push(format!("{}:{}", old, new));
        }
        (Some(change), None) => {
            args.push("-c".to_string());
            args.push(change.to_string());
        }
        (None, Some(_)) => {
            return Err(SvnError::InvalidArguments(
                "新版本不能在没有旧版本的情况下单独指定".to_string(),
            ));
        }
        (None, None) => {}
    }

    append_targets(&mut args, &[normalize_diff_target(file)]);
    Ok(args)
}

/// Continues a checkout that was cancelled or interrupted.
///
/// `svn update --set-depth infinity` is the right operation: it resumes from the
/// item that is missing, and it also lifts a working copy that the salvage pass
/// created at `--depth immediates` back to full depth. Re-running `checkout`
/// would not, because it honours the sticky depth the partial copy was left with.
pub async fn resume_checkout<F>(path: &str, sink: F) -> Result<CheckoutReport, SvnError>
where
    F: Fn(StreamKind, String) + Send + Sync + Clone + 'static,
{
    let root = std::fs::canonicalize(path)
        .map_err(|error| SvnError::CommandFailed(format!("无法访问目标目录：{error}")))?;
    if !root.join(".svn").exists() {
        return Err(SvnError::InvalidArguments(
            "目标目录不是工作副本，无法继续检出；请重新开始检出。".to_string(),
        ));
    }

    let plan = checkout_plan(false);
    let total = plan.len();
    let mut last_error = None;
    for (index, (cleanup, options)) in plan.iter().enumerate() {
        if index > 0 {
            sink(
                StreamKind::Status,
                format!("第 {}/{} 次尝试：继续获取缺失内容", index + 1, total),
            );
            if *cleanup {
                sink(StreamKind::Status, "svn cleanup".to_string());
                let _ = execute_svn(&["cleanup"], Some(path)).await;
            }
        }
        let mut args: Vec<String> = vec![
            "update".to_string(),
            "--set-depth".to_string(),
            "infinity".to_string(),
            "--accept".to_string(),
            "postpone".to_string(),
        ];
        for (key, value) in *options {
            args.push("--config-option".to_string());
            args.push(format!("{key}={value}"));
        }
        let result =
            stream_with_lock_recovery(&args, Some(path), SVN_NETWORK_TIMEOUT, &sink).await?;
        if result.success {
            return Ok(CheckoutReport {
                revision: updated_revision(&result.stdout),
                output: result.stdout,
                attempts: (index + 1) as u32,
                salvaged: false,
                failed: Vec::new(),
                missing_files: Vec::new(),
            });
        }
        let message = format!("{}\n{}", result.stdout, result.stderr);
        if index + 1 == total || !is_stream_failure(&message) {
            // Nothing more the retry ladder can do. A storage fault still leaves
            // the rest of the tree fetchable, so fall back to the salvage pass,
            // taking the URL from the working copy itself.
            if is_server_storage_failure(&message) {
                if let Ok(url) = execute_svn(&["info", "--show-item", "url"], Some(path)).await {
                    let url = url.trim().to_string();
                    if !url.is_empty() {
                        sink(
                            StreamKind::Status,
                            "检测到服务器无法提供部分文件，自动改为跳过失败项继续检出".to_string(),
                        );
                        let salvage = checkout_salvage(&url, path, sink.clone()).await?;
                        return Ok(CheckoutReport {
                            output: salvage.output,
                            attempts: (index + 1) as u32,
                            revision: None,
                            salvaged: true,
                            failed: salvage.failed,
                            missing_files: salvage.missing_files,
                        });
                    }
                }
            }
            return Err(SvnError::CommandFailed(message));
        }
        last_error = Some(SvnError::CommandFailed(message));
    }
    Err(last_error.unwrap_or_else(|| SvnError::CommandFailed("继续检出失败".to_string())))
}

/// Revision reported by a finished update: "At revision 12." / "Updated to revision 12."
fn updated_revision(output: &str) -> Option<u64> {
    output.lines().rev().find_map(|line| {
        let trimmed = line.trim();
        let rest = trimmed
            .strip_prefix("Updated to revision ")
            .or_else(|| trimmed.strip_prefix("At revision "))?;
        rest.trim_end_matches('.').trim().parse().ok()
    })
}

/// Outcome of a salvage checkout: which directories could not be fetched whole.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SalvageReport {
    /// Directories left incomplete because the server could not serve them.
    pub failed: Vec<String>,
    /// Files the server could not deliver, relative to the working copy root.
    pub missing_files: Vec<String>,
    pub output: String,
}

/// SVN reserves every name ending in `.svn` (and `_svn` on Windows). Such a
/// path can never be a working copy target: passing one makes SVN reply
/// "Skipping argument: E200025: ... ends in a reserved name".
///
/// The working copy's own `.svn` directory is the usual offender, because it
/// shows up when enumerating a directory's children.
pub fn is_reserved_name(name: &str) -> bool {
    let lowered = name.to_ascii_lowercase();
    lowered == ".svn" || lowered == "_svn" || lowered.ends_with(".svn") || lowered.ends_with("_svn")
}

/// True when SVN refused a target only because of a reserved name. That is a
/// skip, not a checkout failure.
pub fn is_reserved_name_error(message: &str) -> bool {
    let lowered = message.to_ascii_lowercase();
    lowered.contains("e200025")
        || lowered.contains("ends in a reserved name")
        // A repository entry that IS `.svn` cannot be created in a working copy.
        || lowered.contains("as the administrative directory")
}

/// Counts files under a directory, cheaply enough to compare before and after a
/// retry. The walk stops at a ceiling so a pathological tree cannot hang it.
fn count_files(directory: &Path) -> usize {
    const LIMIT: usize = 200_000;
    let mut count = 0;
    let mut stack = vec![directory.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.file_name() == ".svn" {
                continue;
            }
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => stack.push(entry.path()),
                Ok(kind) if kind.is_file() => {
                    count += 1;
                    if count >= LIMIT {
                        return count;
                    }
                }
                _ => {}
            }
        }
    }
    count
}

/// Arguments that fetch a directory's immediate entries without touching the
/// persistent depth. `--set-depth` must never appear here: lowering the sticky
/// depth makes SVN delete the subtree, which is how a salvage pass once wiped
/// the content it had just downloaded.
fn salvage_entry_args(label: &str) -> Vec<String> {
    vec![
        "update".to_string(),
        "--depth".to_string(),
        "immediates".to_string(),
        "--accept".to_string(),
        "postpone".to_string(),
        "--".to_string(),
        label.to_string(),
    ]
}

/// Decodes `%XX` escapes so a URL typed with non-ASCII characters compares equal
/// to the form SVN records. SVN stores URIs percent-encoded, so a checkout of
/// `.../2.业务` is recorded as `.../2.%E4%B8%9A%E5%8A%A1`.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let high = (bytes[index + 1] as char).to_digit(16);
            let low = (bytes[index + 2] as char).to_digit(16);
            if let (Some(high), Some(low)) = (high, low) {
                decoded.push((high * 16 + low) as u8);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

/// Canonical form used only for comparison: lower-case scheme and authority,
/// decoded path, no trailing slash.
fn normalize_repository_url(url: &str) -> String {
    let trimmed = url.trim();
    let Some((scheme, rest)) = trimmed.split_once("://") else {
        return trimmed.trim_end_matches('/').to_string();
    };
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, ""),
    };
    let path = percent_decode(path);
    let path = path.trim_end_matches('/');
    format!(
        "{}://{}{}",
        scheme.to_ascii_lowercase(),
        authority.to_ascii_lowercase(),
        path
    )
}

/// True when both URLs address the same repository location, ignoring the
/// escaping and case differences SVN itself introduces.
pub fn same_repository_url(left: &str, right: &str) -> bool {
    normalize_repository_url(left) == normalize_repository_url(right)
}

/// Refuses to touch a directory that already belongs to another repository:
/// re-pointing it would mix two trees and could delete files.
async fn ensure_target_matches(url: &str, path: &str) -> Result<(), SvnError> {
    if !Path::new(path).join(".svn").exists() {
        return Ok(());
    }
    let output = execute_svn(&["info", "--show-item", "url"], Some(path)).await;
    let Ok(existing) = output else {
        // A broken or half-created working copy is repaired by the checkout.
        return Ok(());
    };
    let existing = existing.trim();
    if existing.is_empty() || same_repository_url(existing, url) {
        return Ok(());
    }
    Err(SvnError::InvalidArguments(format!(
        "目标目录已是另一个仓库地址的工作副本：{existing}（本次检出使用 {url}）。请换一个空目录，或改用该地址检出。"
    )))
}

/// Immediate children of a repository directory, read from the server metadata.
/// Listings keep working when a file's content blob is unreadable, so they are
/// the only reliable way to describe what a failed directory should contain.
async fn remote_entries(url: &str, relative: &Path) -> Result<Vec<(String, bool)>, SvnError> {
    let mut target = url.trim_end_matches('/').to_string();
    if !relative.as_os_str().is_empty() {
        for component in relative.components() {
            if let std::path::Component::Normal(part) = component {
                target.push('/');
                target.push_str(&part.to_string_lossy());
            }
        }
    }
    let args = vec![
        "list".to_string(),
        "--xml".to_string(),
        "--depth".to_string(),
        "immediates".to_string(),
        target,
    ];
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = execute_svn(&refs, None).await?;
    let mut entries = Vec::new();
    for entry in parse_status_list_xml(&output) {
        entries.push(entry);
    }
    Ok(entries)
}

/// Parses `svn list --xml`: `(name, is_directory)` pairs.
fn parse_status_list_xml(xml: &str) -> Vec<(String, bool)> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut buffer = Vec::new();
    let mut entries = Vec::new();
    let mut in_entry = false;
    let mut name = String::new();
    let mut is_dir = false;
    let mut in_name = false;
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(ref event)) | Ok(Event::Empty(ref event)) => {
                match event.name().as_ref() {
                    b"entry" => {
                        in_entry = true;
                        name.clear();
                        is_dir = false;
                        for attribute in event.attributes().flatten() {
                            if attribute.key.as_ref() == b"kind" {
                                is_dir = attribute.value.as_ref() == b"dir";
                            }
                        }
                    }
                    b"name" if in_entry => in_name = true,
                    _ => {}
                }
            }
            Ok(Event::Text(ref event)) if in_name => {
                name.push_str(&event.unescape().unwrap_or_default());
            }
            Ok(Event::End(ref event)) => match event.name().as_ref() {
                b"name" => in_name = false,
                b"entry" if in_entry => {
                    if !name.is_empty() {
                        entries.push((name.clone(), is_dir));
                    }
                    in_entry = false;
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buffer.clear();
    }
    entries
}

const SALVAGE_MAX_DEPTH: usize = 12;
/// A failing directory is fetched again this many times while it keeps growing.
const SALVAGE_PASSES: usize = 4;

/// Fetches everything the server is still able to serve, descending only into
/// the directories that fail. A damaged revision file makes one subtree
/// unreadable; the rest of the working copy is still usable.
pub async fn checkout_salvage<F>(url: &str, path: &str, sink: F) -> Result<SalvageReport, SvnError>
where
    F: Fn(StreamKind, String) + Send + Sync + Clone + 'static,
{
    ensure_target_matches(url, path).await?;
    if !Path::new(path).join(".svn").exists() {
        sink(
            StreamKind::Status,
            "第 1 步：只取目录骨架（不下载文件内容）".to_string(),
        );
        let args = [
            "checkout".to_string(),
            "--depth".to_string(),
            "immediates".to_string(),
            url.to_string(),
            path.to_string(),
        ];
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let result = execute_svn_streaming(&refs, None, SVN_NETWORK_TIMEOUT, sink.clone()).await?;
        if !result.success {
            let message = format!("{}\n{}", result.stdout, result.stderr);
            // A repository that versions a `.svn` directory breaks the skeleton
            // checkout only for that entry; the rest is still usable.
            if !is_reserved_name_error(&message) {
                return Err(SvnError::CommandFailed(message));
            }
            sink(
                StreamKind::Status,
                "仓库中存在以 .svn 命名的保留条目，已跳过该条目继续".to_string(),
            );
        }
    }
    sink(
        StreamKind::Status,
        "第 2 步：逐目录取全，失败处继续向下拆分".to_string(),
    );

    let mut failed = Vec::new();
    let mut missing_files = Vec::new();
    let output = fill_salvage(
        path,
        url,
        Path::new(""),
        &sink,
        &mut failed,
        &mut missing_files,
        0,
    )
    .await?;
    Ok(SalvageReport {
        failed,
        missing_files,
        output,
    })
}

/// Runs one `svn update --set-depth infinity` for `relative`; on a server
/// storage failure it keeps what arrived, recreates the child directories and
/// recurses, so only the unreadable leaf stays missing.
fn fill_salvage<'a, F>(
    root: &'a str,
    url_hint: &'a str,
    relative: &'a Path,
    sink: &'a F,
    failed: &'a mut Vec<String>,
    missing_files: &'a mut Vec<String>,
    depth: usize,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, SvnError>> + Send + 'a>>
where
    F: Fn(StreamKind, String) + Send + Sync + Clone + 'static,
{
    Box::pin(async move {
        let label = if relative.as_os_str().is_empty() {
            ".".to_string()
        } else {
            relative.to_string_lossy().into_owned()
        };
        let args: Vec<String> = vec![
            "update".to_string(),
            "--set-depth".to_string(),
            "infinity".to_string(),
            "--accept".to_string(),
            "postpone".to_string(),
            // Per-file transfers fail one item at a time instead of cutting a
            // single huge report, which is what lets the retries below converge.
            "--config-option".to_string(),
            "servers:global:http-bulk-updates=no".to_string(),
            "--".to_string(),
            label.clone(),
        ];
        let local = if relative.as_os_str().is_empty() {
            PathBuf::from(root)
        } else {
            Path::new(root).join(relative)
        };
        let mut result =
            stream_with_lock_recovery(&args, Some(root), SVN_NETWORK_TIMEOUT, sink).await?;
        // Each pass resumes after the item that failed, so repeating the same
        // command makes monotonic progress until the server has nothing left.
        for attempt in 2..=SALVAGE_PASSES {
            if result.success {
                break;
            }
            // Retrying cannot help: SVN will refuse the same name every time.
            if is_reserved_name_error(&format!("{}\n{}", result.stdout, result.stderr)) {
                break;
            }
            let before = count_files(&local);
            sink(
                StreamKind::Status,
                format!("重新获取 {label}（第 {attempt} 轮），已取到的内容保留"),
            );
            result =
                stream_with_lock_recovery(&args, Some(root), SVN_NETWORK_TIMEOUT, sink).await?;
            if count_files(&local) <= before {
                break;
            }
        }
        if result.success {
            return Ok(result.stdout);
        }
        let message = format!("{}\n{}", result.stdout, result.stderr);
        let reserved = is_reserved_name_error(&message);
        if !reserved && !is_server_storage_failure(&message) && !is_stream_failure(&message) {
            return Err(SvnError::CommandFailed(message));
        }
        if reserved {
            // Keep going inside the directory: everything except the reserved
            // entry is still fetchable, and this is not a server fault.
            sink(
                StreamKind::Status,
                format!("{label} 含保留名条目（.svn），跳过该条目继续向内拆分"),
            );
        } else {
            sink(
                StreamKind::Status,
                format!("无法完整获取 {label}，保留下载到的内容并继续向内拆分"),
            );
            failed.push(label.clone());
        }
        if depth + 1 >= SALVAGE_MAX_DEPTH {
            return Ok(result.stdout);
        }

        // Fetch the child entries so they can be handled one by one. This must be
        // the per-operation `--depth`, never `--set-depth`: lowering the sticky
        // depth makes SVN delete everything below the directory, which would
        // throw away the content that already arrived and start a fetch/delete
        // loop.
        let refs_input = salvage_entry_args(&label);
        let refs: Vec<&str> = refs_input.iter().map(String::as_str).collect();
        let _ = execute_svn_streaming(&refs, Some(root), SVN_TIMEOUT, sink.clone()).await;

        let mut children: Vec<String> = match fs::read_dir(&local) {
            Ok(entries) => entries
                .flatten()
                .filter(|entry| entry.path().is_dir())
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                // `.svn` lives in every working copy directory and is not a
                // valid target; recursing into it produced the reserved-name
                // warnings once per directory.
                .filter(|name| !is_reserved_name(name))
                .collect(),
            Err(_) => Vec::new(),
        };
        children.sort();

        // Describe exactly what this directory should contain but does not.
        let mut collected = result.stdout;
        if let Ok(remote) = remote_entries(url_hint, relative).await {
            for (name, is_directory) in remote {
                if is_directory {
                    continue;
                }
                // A versioned name ending in `.svn` can never be checked out;
                // reporting it as missing would just be permanent noise.
                if is_reserved_name(&name) {
                    continue;
                }
                if !local.join(&name).exists() {
                    let missing = if relative.as_os_str().is_empty() {
                        name.clone()
                    } else {
                        relative.join(&name).to_string_lossy().into_owned()
                    };
                    if !missing_files.contains(&missing) {
                        missing_files.push(missing);
                    }
                }
            }
        }
        for child in children {
            let child_relative = if relative.as_os_str().is_empty() {
                PathBuf::from(&child)
            } else {
                relative.join(&child)
            };
            collected.push_str(
                &fill_salvage(
                    root,
                    url_hint,
                    &child_relative,
                    sink,
                    failed,
                    missing_files,
                    depth + 1,
                )
                .await?,
            );
        }
        Ok(collected)
    })
}

/// Outcome of a streamed checkout, reported once the process ends.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckoutReport {
    pub output: String,
    pub attempts: u32,
    pub revision: Option<u64>,
    /// True when the checkout switched to the salvage pass because the server
    /// could not serve part of the tree.
    pub salvaged: bool,
    pub failed: Vec<String>,
    pub missing_files: Vec<String>,
}

/// Failures that usually mean the server's XML response was cut short rather
/// than a real repository error: a proxy or timeout truncating a large report.
/// Retrying normally succeeds, and a partly checked-out directory resumes.
/// A server-side storage fault: the repository file itself cannot be read. No
/// client-side setting can work around it, but the rest of the tree can still be
/// checked out, so it is worth reporting separately from a transport failure.
pub fn is_server_storage_failure(message: &str) -> bool {
    let lowered = message.to_ascii_lowercase();
    lowered.contains("e000005")
        || lowered.contains("input/output error")
        || lowered.contains("can't read file")
        || lowered.contains("cannot read file")
}

pub fn is_stream_failure(message: &str) -> bool {
    if is_server_storage_failure(message) || is_reserved_name_error(message) {
        return false;
    }
    let lowered = message.to_ascii_lowercase();
    [
        "e175009",
        "e130003",
        "e120171",
        "e170013",
        "e175002",
        "malformed xml",
        "invalid xml",
        "connection reset",
        "connection timed out",
        "timed out",
    ]
    .iter()
    .any(|needle| lowered.contains(needle))
}

/// First `A`/`U`/`D` marker letter of a checkout line, for progress counters.
#[cfg(test)]
fn collapse_marker(line: &str) -> Option<&str> {
    let rest = line.get(1..)?;
    if !rest.starts_with(' ') {
        return None;
    }
    line.get(0..1)
}

fn checked_out_revision(output: &str) -> Option<u64> {
    // "Checked out revision 7394."
    output.lines().rev().find_map(|line| {
        let rest = line.trim().strip_prefix("Checked out revision ")?;
        rest.trim_end_matches('.').trim().parse().ok()
    })
}

/// Options for the later attempts. `http-bulk-updates=no` is the important one:
/// it stops the server from inlining file contents into one huge update report,
/// which is what gets truncated on large checkouts, and fetches each file in its
/// own request instead.
const SKELTA_OPTIONS: &[(&str, &str)] = &[("servers:global:http-bulk-updates", "no")];
const SKELTA_OPTIONS_NO_COMPRESSION: &[(&str, &str)] = &[
    ("servers:global:http-bulk-updates", "no"),
    ("servers:global:http-compression", "no"),
    ("servers:global:http-timeout", "900"),
];

/// A working copy left locked by an interrupted SVN command. `svn cleanup` is the
/// documented remedy, so the app performs it instead of asking the user to find
/// a cleanup entry.
pub fn is_work_copy_locked(message: &str) -> bool {
    let lowered = message.to_ascii_lowercase();
    lowered.contains("e155004")
        || lowered.contains("e155037")
        || lowered.contains("working copy")
            && (lowered.contains("locked") || lowered.contains("cleanup"))
        || lowered.contains("is already locked")
        || lowered.contains("database is locked")
}

/// Streaming commands recover once from a lock: clean up, then try again.
async fn stream_with_lock_recovery<F>(
    args: &[String],
    cwd: Option<&str>,
    deadline: Duration,
    sink: &F,
) -> Result<super::executor::StreamResult, SvnError>
where
    F: Fn(StreamKind, String) + Send + Sync + Clone + 'static,
{
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let first = execute_svn_streaming(&refs, cwd, deadline, sink.clone()).await?;
    if first.success {
        return Ok(first);
    }
    let message = format!("{}\n{}", first.stdout, first.stderr);
    if !is_work_copy_locked(&message) {
        return Ok(first);
    }
    sink(
        StreamKind::Status,
        "工作副本被锁定：自动执行 svn cleanup 后重试".to_string(),
    );
    let _ = execute_svn(&["cleanup"], cwd).await;
    execute_svn_streaming(&refs, cwd, deadline, sink.clone()).await
}

/// Non-streaming commands take the same lock recovery.
async fn network_with_lock_recovery(args: &[String], path: &str) -> Result<String, SvnError> {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let first = execute_svn_network(&refs, Some(path)).await;
    match first {
        Ok(output) => Ok(output),
        Err(error) if is_work_copy_locked(&error.to_string()) => {
            let _ = execute_svn(&["cleanup"], Some(path)).await;
            execute_svn_network(&refs, Some(path)).await
        }
        Err(error) => Err(error),
    }
}

type Attempt = (bool, &'static [(&'static str, &'static str)]);

/// The first attempt lets the server use bulk updates because it is faster.
/// A repository that already truncated a response skips straight to the
/// per-file mode, so the user does not transfer the same broken report again.
fn checkout_plan(skip_bulk: bool) -> Vec<Attempt> {
    let mut plan: Vec<Attempt> = Vec::new();
    if !skip_bulk {
        plan.push((false, &[]));
    }
    plan.push((true, SKELTA_OPTIONS));
    plan.push((true, SKELTA_OPTIONS_NO_COMPRESSION));
    plan
}

fn bulk_registry() -> &'static std::sync::Mutex<HashSet<String>> {
    static REGISTRY: std::sync::OnceLock<std::sync::Mutex<HashSet<String>>> =
        std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| std::sync::Mutex::new(HashSet::new()))
}

/// `scheme://authority` of an HTTP(S) URL; other transports have no bulk mode.
pub fn url_authority(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let authority = rest.split('/').next()?.trim();
    if authority.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{}", authority.to_ascii_lowercase()))
}

fn bulk_updates_unsafe(url: &str) -> bool {
    let Some(authority) = url_authority(url) else {
        return false;
    };
    bulk_registry()
        .lock()
        .map(|known| known.contains(&authority))
        .unwrap_or(false)
}

fn remember_bulk_truncation(url: &str) {
    if let Some(authority) = url_authority(url) {
        if let Ok(mut known) = bulk_registry().lock() {
            known.insert(authority);
        }
    }
}

#[cfg(test)]
fn forget_bulk_truncation(url: &str) {
    if let Some(authority) = url_authority(url) {
        if let Ok(mut known) = bulk_registry().lock() {
            known.remove(&authority);
        }
    }
}

/// Checks out while reporting every line, and retries a truncated response.
/// A failed attempt leaves a partial working copy that the next one resumes.
pub async fn checkout_streaming<F>(
    url: &str,
    path: &str,
    revision: Option<u64>,
    sink: F,
) -> Result<CheckoutReport, SvnError>
where
    F: Fn(StreamKind, String) + Send + Sync + Clone + 'static,
{
    ensure_target_matches(url, path).await?;
    let skip_bulk = bulk_updates_unsafe(url);
    if skip_bulk {
        sink(
            StreamKind::Status,
            "该仓库此前出现过响应截断，本次直接使用逐文件传输（较慢但更可靠）".to_string(),
        );
    }
    let plan = checkout_plan(skip_bulk);
    let total = plan.len();
    let mut last_error = None;
    for (index, (cleanup, options)) in plan.iter().enumerate() {
        if index > 0 {
            let hint = if options.is_empty() {
                ""
            } else {
                "，改为逐文件传输（较慢但更可靠）"
            };
            sink(
                StreamKind::Status,
                format!(
                    "第 {}/{} 次尝试{hint}：保留已检出内容继续",
                    index + 1,
                    total
                ),
            );
            if *cleanup {
                sink(StreamKind::Status, "svn cleanup".to_string());
                let _ = execute_svn(&["cleanup"], Some(path)).await;
            }
        }

        let mut args: Vec<String> = vec!["checkout".to_string()];
        if let Some(rev) = revision {
            args.push("-r".to_string());
            args.push(rev.to_string());
        }
        for (key, value) in *options {
            args.push("--config-option".to_string());
            args.push(format!("{key}={value}"));
        }
        args.push(url.to_string());
        args.push(path.to_string());
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();

        let result = execute_svn_streaming(&refs, None, SVN_NETWORK_TIMEOUT, sink.clone()).await?;
        if result.success {
            return Ok(CheckoutReport {
                revision: checked_out_revision(&result.stdout),
                output: result.stdout,
                attempts: (index + 1) as u32,
                salvaged: false,
                failed: Vec::new(),
                missing_files: Vec::new(),
            });
        }

        let message = format!("{}\n{}", result.stdout, result.stderr);
        let error = SvnError::CommandFailed(message);
        if !is_stream_failure(&error.to_string()) || index + 1 == total {
            return Err(error);
        }
        // Remember the host so the next checkout starts in per-file mode instead
        // of pulling the same enormous report again.
        remember_bulk_truncation(url);
        sink(
            StreamKind::Status,
            "响应被截断，保留已检出内容并重试".to_string(),
        );
        last_error = Some(error);
    }
    Err(last_error.unwrap_or_else(|| SvnError::CommandFailed("检出失败".to_string())))
}

pub async fn update(path: &str, revision: Option<u64>) -> Result<String, SvnError> {
    gate::exclusive(Path::new(path), async {
        // A large update inlines file contents the same way a checkout does, so
        // it gets the same ladder: retry in per-file mode on a truncated report.
        let plan = checkout_plan(false);
        let total = plan.len();
        let mut last_error = None;
        for (index, (cleanup, options)) in plan.iter().enumerate() {
            if index > 0 && *cleanup {
                let _ = execute_svn(&["cleanup"], Some(path)).await;
            }
            let mut args: Vec<String> = vec!["update".to_string()];
            if let Some(rev) = revision {
                args.push("-r".to_string());
                args.push(rev.to_string());
            }
            for (key, value) in *options {
                args.push("--config-option".to_string());
                args.push(format!("{key}={value}"));
            }
            match network_with_lock_recovery(&args, path).await {
                Ok(output) => return Ok(output),
                Err(error) => {
                    if index + 1 == total || !is_stream_failure(&error.to_string()) {
                        return Err(error);
                    }
                    last_error = Some(error);
                }
            }
        }
        Err(last_error.unwrap_or_else(|| SvnError::CommandFailed("更新失败".to_string())))
    })
    .await
}

fn resolve_workspace_file_path(workspace: &str, file: &str) -> Result<PathBuf, SvnError> {
    let workspace_path = Path::new(workspace)
        .canonicalize()
        .map_err(|e| SvnError::CommandFailed(format!("无法访问工作副本目录：{}", e)))?;
    let file_path = workspace_path.join(file);
    let canonical_file = file_path
        .canonicalize()
        .map_err(|e| SvnError::CommandFailed(format!("无法访问文件：{}", e)))?;

    if !canonical_file.starts_with(&workspace_path) {
        return Err(SvnError::InvalidArguments(
            "文件必须位于当前工作副本内".to_string(),
        ));
    }

    Ok(canonical_file)
}

fn build_unversioned_file_diff(file: &str, content: &str) -> String {
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    let has_trailing_newline = normalized.ends_with('\n');
    let mut lines: Vec<&str> = normalized.split('\n').collect();
    if has_trailing_newline {
        lines.pop();
    }

    let mut diff = String::new();
    diff.push_str(&format!("Index: {}\n", file));
    diff.push_str("===================================================================\n");
    diff.push_str(&format!("--- {}\t(nonexistent)\n", file));
    diff.push_str(&format!("+++ {}\t(working copy)\n", file));
    diff.push_str(&format!("@@ -0,0 +1,{} @@\n", lines.len()));
    for line in &lines {
        diff.push('+');
        diff.push_str(line);
        diff.push('\n');
    }
    if !has_trailing_newline && !lines.is_empty() {
        diff.push_str("\\ No newline at end of file\n");
    }
    diff
}

async fn is_unversioned_file(workspace: &str, file: &str) -> Result<bool, SvnError> {
    let target = normalize_diff_target(file);
    let args = vec!["status", "--xml", "--", target.as_str()];
    let output = execute_svn(&args, Some(workspace)).await?;
    let statuses = parse_status_xml(&output)?;
    let normalized_file = normalize_svn_path(file);
    Ok(statuses.iter().any(|status| {
        status.status_code == "unversioned" && normalize_svn_path(&status.path) == normalized_file
    }) || (statuses.len() == 1 && statuses[0].status_code == "unversioned"))
}

pub async fn remote_info(path: &str) -> Result<SvnInfo, SvnError> {
    let args = vec!["info", "-r", "HEAD", "--xml"];
    let output = execute_svn(&args, Some(path)).await?;
    parse_info_xml(&output)
}

pub async fn commit(
    path: &str,
    message: &str,
    files: Option<&[String]>,
) -> Result<String, SvnError> {
    gate::exclusive(Path::new(path), commit_locked(path, message, files)).await
}

async fn commit_locked(
    path: &str,
    message: &str,
    files: Option<&[String]>,
) -> Result<String, SvnError> {
    let mut args: Vec<String> = vec!["commit".to_string(), "-m".to_string(), message.to_string()];

    if let Some(file_list) = files {
        if file_list.is_empty() {
            return Err(SvnError::InvalidArguments(
                "显式文件列表不能为空；省略文件列表可提交整个工作副本".to_string(),
            ));
        }
        // SVN directory deletion/replacement is atomic even with --depth empty.
        // Validate against fresh status so deselected descendants cannot slip in.
        let statuses = status(path).await?;
        let selected: HashSet<String> = file_list
            .iter()
            .map(|file| {
                normalize_svn_path(file)
                    .trim_start_matches("./")
                    .trim_end_matches('/')
                    .to_string()
            })
            .collect();
        for entry in &statuses {
            let target = normalize_svn_path(&entry.path);
            if !selected.contains(&target) {
                continue;
            }
            if entry.history && Path::new(path).join(&entry.path).is_dir() {
                return Err(SvnError::InvalidArguments(format!(
                    "复制或移动的目录 {} 包含隐式子项，暂不支持通过部分提交处理；请使用 SVN 客户端提交完整的复制或移动操作", entry.path
                )));
            }
            if matches!(entry.status_code.as_str(), "deleted" | "replaced") {
                let prefix = format!("{}/", target);
                if statuses.iter().any(|child| {
                    let child_path = normalize_svn_path(&child.path);
                    child_path.starts_with(&prefix) && !selected.contains(&child_path)
                }) {
                    return Err(SvnError::InvalidArguments(format!(
                        "目录 {} 的删除或替换必须包含全部子项，请完整勾选该目录或取消该目录的选择",
                        entry.path
                    )));
                }
            }
        }
        args.extend(["--depth".to_string(), "empty".to_string()]);
        append_targets(&mut args, file_list);
    }

    let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    execute_svn_network(&args_refs, Some(path)).await
}

pub async fn status(path: &str) -> Result<Vec<SvnStatus>, SvnError> {
    let output = execute_svn(&["status", "--xml"], Some(path)).await?;
    let mut entries = parse_status_xml(&output)?;
    // Default status collapses deleted directories. Expand only affected
    // targets, avoiding a second verbose traversal of the whole workspace.
    let targets: Vec<_> = entries
        .iter()
        .filter(|entry| matches!(entry.status_code.as_str(), "deleted" | "replaced"))
        .map(|entry| normalize_diff_target(&entry.path))
        .collect();
    for group in targets.chunks(16) {
        let mut args: Vec<String> = ["status", "--xml", "--verbose", "--"]
            .iter()
            .map(|arg| arg.to_string())
            .collect();
        args.extend(group.iter().cloned());
        let refs: Vec<_> = args.iter().map(String::as_str).collect();
        let output = execute_svn(&refs, Some(path)).await?;
        entries.extend(parse_status_xml(&output)?);
    }
    let mut changed = std::collections::BTreeMap::new();
    for entry in entries {
        if !matches!(entry.status_code.as_str(), "normal" | "none")
            || matches!(entry.prop_status.as_str(), "modified" | "conflicted")
        {
            changed.insert(entry.path.clone(), entry);
        }
    }
    Ok(collapse_name_mismatches(
        Path::new(path),
        changed.into_values().collect(),
    ))
}

fn normalized(path: &str) -> String {
    path.nfc().collect()
}

/// SVN compares the directory listing with `wc.db` byte for byte. A macOS
/// filesystem can keep a name in NFD while the repository recorded NFC, so one
/// file surfaces twice: an `unversioned` NFD entry and a `missing` NFC entry.
/// Collapse such a pair into a single `name_mismatch` entry that the interface
/// can explain and repair, instead of reporting ten phantom deletions.
///
/// Idempotent: entries already collapsed are left alone, so it is safe to run
/// after merging a scoped refresh into the cached list.
pub fn collapse_name_mismatches(root: &Path, entries: Vec<SvnStatus>) -> Vec<SvnStatus> {
    let unversioned: HashMap<String, Vec<usize>> = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.status_code == "unversioned")
        .fold(HashMap::new(), |mut index, (position, entry)| {
            index
                .entry(normalized(&entry.path))
                .or_default()
                .push(position);
            index
        });

    let mut paired_unversioned = HashSet::new();
    let mut mismatched_missing = HashSet::new();
    for (position, entry) in entries.iter().enumerate() {
        if entry.status_code != "missing" {
            continue;
        }
        let Some(candidates) = unversioned.get(&normalized(&entry.path)) else {
            continue;
        };
        // A canonically equal name that differs byte-wise is the same file.
        if let Some(&twin) = candidates
            .iter()
            .find(|&&twin| entries[twin].path != entry.path)
        {
            mismatched_missing.insert(position);
            paired_unversioned.insert(twin);
        }
    }

    // SVN does not always report the on-disk twin as unversioned: for a file it
    // may report only `missing` while a canonically equal name sits in the same
    // directory. Look at the directory itself in that case.
    let mut directories: HashMap<PathBuf, HashMap<String, String>> = HashMap::new();
    for (position, entry) in entries.iter().enumerate() {
        if entry.status_code != "missing" || mismatched_missing.contains(&position) {
            continue;
        }
        let path = Path::new(&entry.path);
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            continue;
        };
        let wanted = name.to_string_lossy().into_owned();
        let names = directories.entry(parent.to_path_buf()).or_insert_with(|| {
            let mut found = HashMap::new();
            if let Ok(listing) = fs::read_dir(root.join(parent)) {
                for item in listing.flatten() {
                    let actual = item.file_name().to_string_lossy().into_owned();
                    found.entry(normalized(&actual)).or_insert(actual);
                }
            }
            found
        });
        if names
            .get(&normalized(&wanted))
            .is_some_and(|actual| *actual != wanted)
        {
            mismatched_missing.insert(position);
        }
    }

    entries
        .into_iter()
        .enumerate()
        .filter_map(|(position, mut entry)| {
            if paired_unversioned.contains(&position) {
                return None;
            }
            if mismatched_missing.contains(&position) {
                entry.status = "name_mismatch".to_string();
                entry.status_code = "name_mismatch".to_string();
            }
            Some(entry)
        })
        .collect()
}

/// Query only exact paths. The filesystem event queue chooses these
/// targets; a complete status scan remains the fallback for unknown scope.
pub(super) async fn status_for_paths(
    path: &str,
    paths: &[String],
) -> Result<Vec<SvnStatus>, SvnError> {
    let mut entries = Vec::new();
    // Batch the identified paths; oversized platform argv limits fall back
    // to a complete scan in the caller.
    for group in paths.chunks(64) {
        let mut args: Vec<String> = ["status", "--xml", "--depth", "empty", "--"]
            .iter()
            .map(|arg| arg.to_string())
            .collect();
        args.extend(group.iter().map(|target| normalize_diff_target(target)));
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = execute_svn(&refs, Some(path)).await?;
        entries.extend(parse_status_xml(&output)?);
    }
    Ok(collapse_name_mismatches(
        Path::new(path),
        entries
            .into_iter()
            .filter(|entry| {
                !matches!(entry.status_code.as_str(), "normal" | "none" | "ignored")
                    || matches!(entry.prop_status.as_str(), "modified" | "conflicted")
            })
            .collect(),
    ))
}

fn build_log_args(
    limit: Option<u32>,
    start_rev: Option<u64>,
    end_rev: Option<u64>,
    date_from: Option<&str>,
    date_to: Option<&str>,
) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "log".to_string(),
        "--xml".to_string(),
        "--verbose".to_string(),
    ];

    if let Some(lim) = limit {
        args.push("-l".to_string());
        args.push(lim.to_string());
    }

    // Scan newest first so filtered batches and their revision cursor agree.
    if date_from.is_some() || date_to.is_some() {
        let newest = if let Some(rev) = start_rev {
            rev.to_string()
        } else if let Some(to) = date_to {
            // The date picker supplies a day, while SVN date revisions use an
            // instant. Include commits throughout the selected end day.
            format!("{{{}T23:59:59.999999}}", to)
        } else {
            "HEAD".to_string()
        };
        let oldest = date_from
            .map(|from| format!("{{{}}}", from))
            .unwrap_or_else(|| "1".to_string());
        args.push("-r".to_string());
        args.push(format!("{}:{}", newest, oldest));
    } else if let Some(start) = start_rev {
        if let Some(end) = end_rev {
            args.push("-r".to_string());
            args.push(format!("{}:{}", start, end));
        } else {
            args.push("-r".to_string());
            args.push(format!("{}:HEAD", start));
        }
    } else {
        args.push("-r".to_string());
        args.push("HEAD:1".to_string());
    }

    args
}

async fn fetch_log_batch(
    path: &str,
    limit: Option<u32>,
    start_rev: Option<u64>,
    end_rev: Option<u64>,
    date_from: Option<&str>,
    date_to: Option<&str>,
) -> Result<Vec<SvnLogEntry>, SvnError> {
    let args = build_log_args(limit, start_rev, end_rev, date_from, date_to);
    let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let output = execute_svn(&args_refs, Some(path)).await?;
    let entries = parse_log_xml(&output)?;
    Ok(entries
        .into_iter()
        .filter(|entry| log_entry_matches_dates(entry, date_from, date_to))
        .collect())
}

fn log_entry_matches_dates(
    entry: &SvnLogEntry,
    date_from: Option<&str>,
    date_to: Option<&str>,
) -> bool {
    if date_from.is_none() && date_to.is_none() {
        return true;
    }
    let Ok(timestamp) = DateTime::parse_from_rfc3339(&entry.date) else {
        return false;
    };
    let local_day = timestamp
        .with_timezone(&Local)
        .format("%Y-%m-%d")
        .to_string();
    date_from.is_none_or(|from| local_day.as_str() >= from)
        && date_to.is_none_or(|to| local_day.as_str() <= to)
}

fn log_entry_matches(entry: &SvnLogEntry, keyword: Option<&str>, author: Option<&str>) -> bool {
    if let Some(author_filter) = author.map(str::trim).filter(|value| !value.is_empty()) {
        if !entry.author.eq_ignore_ascii_case(author_filter) {
            return false;
        }
    }

    let Some(keyword) = keyword.map(str::trim).filter(|value| !value.is_empty()) else {
        return true;
    };
    let keyword = keyword.to_lowercase();

    entry.message.to_lowercase().contains(&keyword)
}

fn append_unique_log_matches(
    matches: &mut Vec<SvnLogEntry>,
    seen_revisions: &mut HashSet<u64>,
    batch: &[SvnLogEntry],
    keyword: Option<&str>,
    author: Option<&str>,
    limit: Option<u32>,
) -> bool {
    for entry in batch {
        if log_entry_matches(entry, keyword, author) && seen_revisions.insert(entry.revision) {
            matches.push(entry.clone());
            if limit.is_some_and(|limit| matches.len() >= limit as usize) {
                return true;
            }
        }
    }

    false
}

async fn filtered_log(
    path: &str,
    limit: Option<u32>,
    start_rev: Option<u64>,
    end_rev: Option<u64>,
    keyword: Option<&str>,
    author: Option<&str>,
    date_from: Option<&str>,
    date_to: Option<&str>,
) -> Result<Vec<SvnLogEntry>, SvnError> {
    let scan_limit = limit.unwrap_or(200).saturating_mul(4).max(200);
    let mut cursor = start_rev;
    let mut matches = Vec::new();
    let mut seen_revisions = HashSet::new();

    loop {
        let batch =
            fetch_log_batch(path, Some(scan_limit), cursor, end_rev, date_from, date_to).await?;
        if batch.is_empty() {
            break;
        }

        if append_unique_log_matches(
            &mut matches,
            &mut seen_revisions,
            &batch,
            keyword,
            author,
            limit,
        ) {
            return Ok(matches);
        }

        let oldest_revision = batch.last().map(|entry| entry.revision).unwrap_or(1);
        if oldest_revision <= 1 || batch.len() < scan_limit as usize {
            break;
        }
        cursor = Some(oldest_revision - 1);
    }

    Ok(matches)
}

pub async fn log(
    path: &str,
    limit: Option<u32>,
    start_rev: Option<u64>,
    end_rev: Option<u64>,
    keyword: Option<&str>,
    author: Option<&str>,
    date_from: Option<&str>,
    date_to: Option<&str>,
) -> Result<Vec<SvnLogEntry>, SvnError> {
    let has_keyword = keyword.is_some_and(|value| !value.trim().is_empty());
    let has_author = author.is_some_and(|value| !value.trim().is_empty());
    if has_keyword || has_author {
        return filtered_log(
            path,
            limit.map(|limit| limit.max(1)),
            start_rev,
            end_rev,
            keyword,
            author,
            date_from,
            date_to,
        )
        .await;
    }

    fetch_log_batch(path, limit, start_rev, end_rev, date_from, date_to).await
}

pub async fn current_user(path: &str) -> Result<Option<SvnAuthUser>, SvnError> {
    let info = info(path).await?;
    let output = execute_svn(&["auth"], Some(path)).await?;
    Ok(parse_auth_user(&output, &info.repository_root))
}

pub async fn info(path: &str) -> Result<SvnInfo, SvnError> {
    let args = vec!["info", "--xml"];
    let output = execute_svn(&args, Some(path)).await?;
    parse_info_xml(&output)
}

pub async fn local_revision(path: &str) -> Result<u64, SvnError> {
    let args = vec!["info", "--show-item", "revision"];
    let output = execute_svn(&args, Some(path)).await?;
    output
        .trim()
        .parse()
        .map_err(|_| SvnError::ParseError(format!("无法解析本地工作副本版本：{}", output.trim())))
}

fn parse_auth_user(output: &str, repository_root: &str) -> Option<SvnAuthUser> {
    let root_key = normalize_url_key(repository_root);
    let host_key = extract_url_host_key(repository_root);

    output
        .split("------------------------------------------------------------------------")
        .filter_map(|block| {
            let mut realm = "";
            let mut username = "";
            for line in block.lines() {
                if let Some(value) = line.strip_prefix("Authentication realm:") {
                    realm = value.trim();
                } else if let Some(value) = line.strip_prefix("Username:") {
                    username = value.trim();
                }
            }
            if username.is_empty() {
                return None;
            }

            let normalized_realm = normalize_url_key(realm);
            let matches_root = !root_key.is_empty() && normalized_realm.contains(&root_key);
            let matches_host = host_key
                .as_ref()
                .is_some_and(|host| normalized_realm.contains(host));
            if matches_root || matches_host {
                Some(SvnAuthUser {
                    username: username.to_string(),
                    realm: realm.to_string(),
                })
            } else {
                None
            }
        })
        .next()
}

fn normalize_url_key(value: &str) -> String {
    value
        .trim()
        .trim_matches('<')
        .trim_matches('>')
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

fn extract_url_host_key(value: &str) -> Option<String> {
    let normalized = normalize_url_key(value);
    let (_, rest) = normalized.split_once("://")?;
    let host = rest.split('/').next()?.trim();
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct BlameLine {
    pub revision: u64,
    pub author: String,
    pub line: String,
}

pub async fn blame(workspace: &str, file: &str) -> Result<Vec<BlameLine>, SvnError> {
    let args = vec!["blame", "--", file];
    let output = execute_svn(&args, Some(workspace)).await?;
    parse_blame_text(&output)
}

pub async fn diff(
    workspace: &str,
    file: &str,
    old_rev: Option<u64>,
    new_rev: Option<u64>,
) -> Result<DiffResult, SvnError> {
    let diff_file = normalize_workspace_diff_file(workspace, file)?;

    if old_rev.is_none() && new_rev.is_none() && is_unversioned_file(workspace, &diff_file).await? {
        let file_path = resolve_workspace_file_path(workspace, &diff_file)?;
        let content = fs::read(&file_path)
            .map_err(|e| SvnError::CommandFailed(format!("无法读取未版本文件：{}", e)))?;
        let content = String::from_utf8_lossy(&content);
        return Ok(DiffResult {
            path: diff_file.clone(),
            diff: build_unversioned_file_diff(&diff_file, content.as_ref()),
            old_revision: 0,
            new_revision: 0,
        });
    }

    let args = build_diff_args(&diff_file, old_rev, new_rev)?;
    let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let output = execute_svn(&args_refs, Some(workspace)).await?;
    let (result_old_rev, result_new_rev) = match (old_rev, new_rev) {
        (Some(old), Some(new)) => (old, new),
        (Some(change), None) => (change.saturating_sub(1), change),
        _ => (0, 0),
    };

    Ok(DiffResult {
        path: diff_file,
        diff: output,
        old_revision: result_old_rev,
        new_revision: result_new_rev,
    })
}

fn patch_output_has_conflicts(output: &str) -> bool {
    output.lines().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("C ")
            || trimmed.contains(".svnpatch.rej")
            || trimmed.to_ascii_lowercase().contains("rejected hunk")
    })
}

pub async fn apply_patch(workspace: &str, patch: &str, reverse: bool) -> Result<String, SvnError> {
    gate::exclusive(
        Path::new(workspace),
        apply_patch_locked(workspace, patch, reverse),
    )
    .await
}

async fn apply_patch_locked(
    workspace: &str,
    patch: &str,
    reverse: bool,
) -> Result<String, SvnError> {
    if patch.trim().is_empty() {
        return Err(SvnError::InvalidArguments("补丁内容不能为空".to_string()));
    }

    let workspace_path = Path::new(workspace)
        .canonicalize()
        .map_err(|e| SvnError::CommandFailed(format!("无法访问工作副本目录：{}", e)))?;
    if !workspace_path.is_dir() {
        return Err(SvnError::InvalidArguments(
            "工作副本路径必须是目录".to_string(),
        ));
    }

    let patch_path = std::env::temp_dir().join(format!(
        "orcasvn-stash-{}-{}.patch",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    ));
    fs::write(&patch_path, patch)
        .map_err(|e| SvnError::CommandFailed(format!("无法创建临时补丁文件：{}", e)))?;

    let mut args = vec!["patch".to_string()];
    if reverse {
        args.push("--reverse-diff".to_string());
    }
    args.push(patch_path.to_string_lossy().into_owned());
    let workspace_cwd = workspace_path.to_string_lossy().into_owned();

    let mut dry_run_args = args.clone();
    dry_run_args.insert(1, "--dry-run".to_string());
    let dry_run_refs: Vec<&str> = dry_run_args.iter().map(|arg| arg.as_str()).collect();
    let dry_run = execute_svn(&dry_run_refs, Some(&workspace_cwd)).await;
    if let Ok(output) = &dry_run {
        if patch_output_has_conflicts(output) {
            let _ = fs::remove_file(&patch_path);
            return Err(SvnError::CommandFailed(
                "补丁与当前工作副本冲突，未应用任何更改".to_string(),
            ));
        }
    }
    if let Err(error) = dry_run {
        let _ = fs::remove_file(&patch_path);
        return Err(error);
    }

    let args_refs: Vec<&str> = args.iter().map(|arg| arg.as_str()).collect();
    let result = execute_svn(&args_refs, Some(&workspace_cwd))
        .await
        .and_then(|output| {
            if patch_output_has_conflicts(&output) {
                Err(SvnError::CommandFailed(
                    "补丁应用时发生冲突，贮藏记录已保留".to_string(),
                ))
            } else {
                Ok(output)
            }
        });
    let _ = fs::remove_file(&patch_path);
    result
}

pub async fn add(path: &str, files: &[String]) -> Result<String, SvnError> {
    gate::exclusive(Path::new(path), async {
        let mut args: Vec<String> = vec!["add".to_string()];
        append_targets(&mut args, files);
        let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        execute_svn(&args_refs, Some(path)).await
    })
    .await
}

pub async fn delete(path: &str, files: &[String]) -> Result<String, SvnError> {
    gate::exclusive(Path::new(path), async {
        let mut args: Vec<String> = vec!["delete".to_string()];
        append_targets(&mut args, files);
        let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        execute_svn(&args_refs, Some(path)).await
    })
    .await
}

pub async fn revert(path: &str, files: &[String]) -> Result<String, SvnError> {
    gate::exclusive(Path::new(path), async {
        let mut args: Vec<String> = vec!["revert".to_string()];
        append_targets(&mut args, files);
        let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        execute_svn(&args_refs, Some(path)).await
    })
    .await
}

pub async fn resolve(path: &str, files: &[String], strategy: &str) -> Result<String, SvnError> {
    gate::exclusive(Path::new(path), async {
        let mut args: Vec<String> = vec![
            "resolve".to_string(),
            "--accept".to_string(),
            strategy.to_string(),
        ];
        append_targets(&mut args, files);
        let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        execute_svn(&args_refs, Some(path)).await
    })
    .await
}

pub async fn cleanup(path: &str) -> Result<String, SvnError> {
    gate::exclusive(Path::new(path), async {
        let args = vec!["cleanup"];
        execute_svn(&args, Some(path)).await
    })
    .await
}

pub async fn switch_cmd(path: &str, url: &str) -> Result<String, SvnError> {
    gate::exclusive(Path::new(path), async {
        let args = vec!["switch", url];
        execute_svn_network(&args, Some(path)).await
    })
    .await
}

/// Renames on-disk entries so they match the repository spelling. Only the name
/// changes: no file content, no SVN state, and nothing is committed.
pub async fn repair_name_mismatches(path: &str, files: &[String]) -> Result<usize, SvnError> {
    let root = Path::new(path)
        .canonicalize()
        .map_err(|error| SvnError::CommandFailed(format!("无法访问工作副本目录：{error}")))?;
    gate::exclusive(Path::new(path), async move { repair_names(&root, files) }).await
}

/// Names of the entries the working copy versions in one directory, including
/// the unchanged ones. `svn status` without `--verbose` only reports differences,
/// which is not enough to tell a local rename from a repository duplicate.
async fn versioned_names(root: &Path, parent: &Path) -> Result<HashSet<String>, SvnError> {
    let target = if parent.as_os_str().is_empty() {
        ".".to_string()
    } else {
        parent.to_string_lossy().into_owned()
    };
    let root = root.to_string_lossy().into_owned();
    let args = vec![
        "status".to_string(),
        "--xml".to_string(),
        "--verbose".to_string(),
        "--depth".to_string(),
        "immediates".to_string(),
        "--".to_string(),
        target,
    ];
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = execute_svn(&refs, Some(&root)).await?;
    Ok(parse_status_xml(&output)?
        .into_iter()
        // An unversioned or ignored neighbour is exactly the *drift* case: it sits
        // on disk but not in the repository. Counting it here would mislabel a
        // local rename as a repository duplicate, and deleting that "duplicate"
        // would remove the only versioned entry.
        .filter(|entry| {
            !matches!(
                entry.status_code.as_str(),
                "unversioned" | "ignored" | "none"
            )
        })
        .filter_map(|entry| {
            entry
                .path
                .rsplit('/')
                .next()
                .map(std::string::ToString::to_string)
        })
        .collect())
}

/// Tells the two causes apart:
///
/// * `name_duplicate` — the working copy versions *both* spellings, i.e. the
///   repository itself holds one file under two names. macOS can only create one
///   of them, so one entry is missing forever. Renaming cannot fix it; the
///   duplicate has to be deleted from version control.
/// * `name_mismatch` — only the on-disk name drifted. Renaming settles it for
///   good.
pub async fn classify_name_duplicates(
    root: &Path,
    entries: &mut [SvnStatus],
) -> Result<(), SvnError> {
    let mut parents: HashMap<String, HashSet<String>> = HashMap::new();
    for entry in entries.iter_mut() {
        if entry.status_code != "name_mismatch" {
            continue;
        }
        let path = Path::new(&entry.path);
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            continue;
        };
        let wanted = normalized(&name.to_string_lossy());
        let mut twin = None;
        if let Ok(listing) = fs::read_dir(root.join(parent)) {
            for item in listing.flatten() {
                let actual = item.file_name().to_string_lossy().into_owned();
                if actual != name.to_string_lossy() && normalized(&actual) == wanted {
                    twin = Some(actual);
                }
            }
        }
        let Some(twin) = twin else { continue };
        let parent_key = parent.to_string_lossy().into_owned();
        if !parents.contains_key(&parent_key) {
            let names = versioned_names(root, parent).await.unwrap_or_default();
            parents.insert(parent_key.clone(), names);
        }
        if parents
            .get(&parent_key)
            .is_some_and(|names| names.contains(&twin))
        {
            entry.status = "name_duplicate".to_string();
            entry.status_code = "name_duplicate".to_string();
        }
    }
    Ok(())
}

/// Deletes the phantom entry from version control while keeping the file that
/// actually exists on disk. The user commits the deletion through the normal
/// commit page, and later checkouts stay clean.
pub async fn delete_duplicate_names(path: &str, files: &[String]) -> Result<String, SvnError> {
    gate::exclusive(Path::new(path), async {
        let mut args: Vec<String> = vec!["delete".to_string(), "--keep-local".to_string()];
        append_targets(&mut args, files);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        execute_svn(&refs, Some(path)).await
    })
    .await
}

enum NameMatch {
    Exact,
    Equivalent(std::ffi::OsString),
}

/// Finds `name` in `directory`: byte-identical, or canonically equal under
/// Unicode normalization. `Path::exists` cannot be used because macOS resolves
/// canonically equal names to the same inode.
fn match_name(directory: &Path, name: &std::ffi::OsStr) -> Result<Option<NameMatch>, SvnError> {
    let wanted = normalized(&name.to_string_lossy());
    let mut equivalent = None;
    for entry in fs::read_dir(directory)
        .map_err(|error| SvnError::CommandFailed(format!("无法读取目录：{error}")))?
    {
        let entry =
            entry.map_err(|error| SvnError::CommandFailed(format!("无法读取目录：{error}")))?;
        let actual = entry.file_name();
        if actual == name {
            return Ok(Some(NameMatch::Exact));
        }
        if normalized(&actual.to_string_lossy()) == wanted {
            equivalent = Some(actual);
        }
    }
    Ok(equivalent.map(NameMatch::Equivalent))
}

/// Renames on-disk entries so they match the repository spelling. Renaming a
/// directory fixes every child, so each component is repaired from the top down.
fn repair_names(root: &Path, files: &[String]) -> Result<usize, SvnError> {
    let mut targets: Vec<&String> = files.iter().collect();
    targets.sort_by_key(|file| file.matches('/').count());
    targets.dedup();

    let mut repaired = 0;
    for file in targets {
        let relative = Path::new(file);
        if relative.is_absolute()
            || relative
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return Err(SvnError::InvalidArguments(format!(
                "工作副本内的相对路径才可修复：{file}"
            )));
        }
        let mut current = root.to_path_buf();
        let mut changed = false;
        let mut resolved = true;
        for component in relative.components() {
            let std::path::Component::Normal(name) = component else {
                resolved = false;
                break;
            };
            match match_name(&current, name)? {
                Some(NameMatch::Exact) => {}
                Some(NameMatch::Equivalent(actual)) => {
                    let source = current.join(&actual);
                    let target = current.join(name);
                    fs::rename(&source, &target).map_err(|error| {
                        SvnError::CommandFailed(format!("无法重命名 {}：{error}", source.display()))
                    })?;
                    changed = true;
                }
                None => {
                    resolved = false;
                    break;
                }
            }
            current.push(name);
        }
        if resolved && changed {
            repaired += 1;
        }
    }
    Ok(repaired)
}

pub async fn merge(
    path: &str,
    source: &str,
    rev_start: u64,
    rev_end: u64,
) -> Result<String, SvnError> {
    gate::exclusive(Path::new(path), async {
        let args = vec![
            "merge".to_string(),
            "-r".to_string(),
            format!("{}:{}", rev_start, rev_end),
            source.to_string(),
        ];
        let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        execute_svn_network(&args_refs, Some(path)).await
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::{
        append_targets, append_unique_log_matches, build_diff_args, build_log_args,
        build_unversioned_file_diff, commit, delete, log_entry_matches, log_entry_matches_dates,
        normalize_diff_target, normalize_svn_path, patch_output_has_conflicts, resolve, status,
        update,
    };
    use crate::models::{SvnLogEntry, SvnLogPath, SvnStatus};
    use crate::svn::executor::SvnError;
    use std::collections::HashSet;
    use std::path::Path;

    #[test]
    fn target_arguments_are_separated_from_options() {
        let mut args = vec!["add".to_string()];
        append_targets(
            &mut args,
            &["-leading-dash.txt".to_string(), "src/main.rs".to_string()],
        );
        assert_eq!(args, ["add", "--", "-leading-dash.txt", "src/main.rs"]);
    }

    #[test]
    fn empty_targets_do_not_add_separator() {
        let mut args = vec!["commit".to_string()];
        append_targets(&mut args, &[]);
        assert_eq!(args, ["commit"]);
    }

    #[test]
    fn patch_conflicts_are_detected_before_pop_removes_the_entry() {
        assert!(patch_output_has_conflicts(
            "C         src/main.rs\n>         rejected hunk @@ -1 +1 @@"
        ));
        assert!(patch_output_has_conflicts("path/to/file.svnpatch.rej"));
        assert!(patch_output_has_conflicts("> rejected hunk @@ -2 +2 @@"));
        assert!(!patch_output_has_conflicts(
            "U         src/main.rs\n> applied hunk with offset 2"
        ));
    }

    #[test]
    fn commit_rejects_an_explicit_empty_file_list() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio runtime");
        let result = runtime.block_on(commit(".", "message", Some(&[])));
        assert!(matches!(result, Err(SvnError::InvalidArguments(_))));
    }

    // Uses an isolated local repository; never touches a user's working copy.
    #[test]
    fn selected_commit_preserves_unselected_children_and_other_directories() {
        use std::process::Command;
        if Command::new("svnadmin").arg("--version").output().is_err() {
            eprintln!("Skipping local repository test: svnadmin is unavailable");
            return;
        }
        let directory = std::env::temp_dir().join(format!(
            "orcasvn-commit-test-{}-{}",
            std::process::id(),
            crate::test_support::unique_temp_suffix()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let repository = directory.join("repository");
        let wc = directory.join("wc");
        let created = Command::new("svnadmin")
            .arg("create")
            .arg(&repository)
            .output()
            .unwrap();
        assert!(created.status.success());
        let url = format!(
            "file:///{}",
            repository
                .to_string_lossy()
                .replace('\\', "/")
                .trim_start_matches('/')
        );
        let run = |args: &[&str]| {
            let output = Command::new("svn")
                .args(args)
                .current_dir(if wc.exists() { &wc } else { &directory })
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).into_owned()
        };
        run(&["checkout", &url, wc.to_str().unwrap()]);
        std::fs::create_dir_all(wc.join("A")).unwrap();
        std::fs::create_dir_all(wc.join("B")).unwrap();
        for name in ["A/selected.txt", "A/keep.txt", "B/keep.txt"] {
            std::fs::write(wc.join(name), "before").unwrap();
        }
        run(&["add", "A", "B"]);
        run(&["commit", "-m", "initial"]);
        for name in ["A/selected.txt", "A/keep.txt", "B/keep.txt"] {
            std::fs::write(wc.join(name), "after").unwrap();
        }
        run(&["propset", "test:property", "changed", "A"]);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime
            .block_on(commit(
                wc.to_str().unwrap(),
                "selected",
                Some(&["A".into(), "A/selected.txt".into()]),
            ))
            .unwrap();
        let remaining = run(&["status"]);
        assert!(remaining.contains("A/keep.txt") || remaining.contains("A\\keep.txt"));
        assert!(remaining.contains("B/keep.txt") || remaining.contains("B\\keep.txt"));
        assert!(!remaining.contains("selected.txt"));
        assert_eq!(run(&["cat", "-r", "HEAD", "A/keep.txt"]), "before");
        assert_eq!(run(&["cat", "-r", "HEAD", "A/selected.txt"]), "after");
        std::fs::create_dir(wc.join("A/new")).unwrap();
        std::fs::write(wc.join("A/new/chosen.txt"), "chosen").unwrap();
        std::fs::write(wc.join("A/new/excluded.txt"), "excluded").unwrap();
        run(&["add", "A/new"]);
        runtime
            .block_on(commit(
                wc.to_str().unwrap(),
                "new directory",
                Some(&["A/new".into(), "A/new/chosen.txt".into()]),
            ))
            .unwrap();
        assert!(run(&["status"]).contains("excluded.txt"));
        assert!(!run(&["list", "-r", "HEAD", "A/new"]).contains("excluded.txt"));
        run(&["update"]);
        run(&["delete", "--force", "A"]);
        let partial_delete = runtime.block_on(commit(
            wc.to_str().unwrap(),
            "partial deletion",
            Some(&["A".into(), "A/selected.txt".into()]),
        ));
        assert!(matches!(partial_delete, Err(SvnError::InvalidArguments(_))));
        assert!(run(&["list", "-r", "HEAD", &url]).contains("A/"));
        let deletions = runtime
            .block_on(super::status(wc.to_str().unwrap()))
            .unwrap()
            .into_iter()
            .filter(|entry| {
                entry.path == "A" || entry.path.starts_with("A/") || entry.path.starts_with("A\\")
            })
            .map(|entry| entry.path)
            .collect::<Vec<_>>();
        runtime
            .block_on(commit(
                wc.to_str().unwrap(),
                "complete deletion",
                Some(&deletions),
            ))
            .unwrap();
        assert!(!run(&["list", "-r", "HEAD", &url]).contains("A/"));
        run(&["copy", "B", "C"]);
        let copied_directory = runtime.block_on(commit(
            wc.to_str().unwrap(),
            "copied directory",
            Some(&["C".into()]),
        ));
        assert!(matches!(
            copied_directory,
            Err(SvnError::InvalidArguments(_))
        ));
        assert!(!run(&["list", "-r", "HEAD", &url]).contains("C/"));
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn diff_builds_working_copy_arguments() {
        assert_eq!(
            build_diff_args("src/main.rs", None, None).unwrap(),
            ["diff", "--", "src/main.rs"]
        );
    }

    #[test]
    fn diff_builds_revision_range_arguments() {
        assert_eq!(
            build_diff_args("src/main.rs", Some(10), Some(12)).unwrap(),
            ["diff", "-r", "10:12", "--", "src/main.rs"]
        );
    }

    #[test]
    fn diff_builds_change_arguments() {
        assert_eq!(
            build_diff_args("https://example.test/repo/file@12", Some(12), None).unwrap(),
            [
                "diff",
                "-c",
                "12",
                "--",
                "https://example.test/repo/file@12"
            ]
        );
    }

    #[test]
    fn diff_rejects_new_revision_without_old_revision() {
        assert!(matches!(
            build_diff_args("src/main.rs", None, Some(12)),
            Err(SvnError::InvalidArguments(_))
        ));
    }

    #[test]
    fn diff_escapes_peg_revision_marker_in_local_path() {
        assert_eq!(
            normalize_diff_target("src/name@domain.txt"),
            "src/name@domain.txt@"
        );
        assert_eq!(
            normalize_diff_target("src/name@domain.txt@"),
            "src/name@domain.txt@"
        );
    }

    #[test]
    fn diff_keeps_url_peg_revision() {
        assert_eq!(
            normalize_diff_target("https://example.test/repo/file@12"),
            "https://example.test/repo/file@12"
        );
    }

    #[test]
    fn normalizes_svn_paths_to_forward_slashes() {
        assert_eq!(normalize_svn_path("src\\main.rs"), "src/main.rs");
    }

    #[test]
    fn keeps_relative_diff_target_normalized() {
        assert_eq!(
            super::normalize_workspace_diff_file(".", "src\\main.rs").unwrap(),
            "src/main.rs"
        );
    }

    #[test]
    fn unversioned_file_diff_is_rendered_as_added_file() {
        let diff = build_unversioned_file_diff("src/new.txt", "hello\nworld\n");

        assert!(diff.contains("Index: src/new.txt\n"));
        assert!(diff.contains("--- src/new.txt\t(nonexistent)\n"));
        assert!(diff.contains("+++ src/new.txt\t(working copy)\n"));
        assert!(diff.contains("@@ -0,0 +1,2 @@\n"));
        assert!(diff.contains("+hello\n+world\n"));
    }

    #[test]
    fn log_keyword_does_not_match_changed_path_or_action_label() {
        let entry = SvnLogEntry {
            revision: 8,
            author: "mark_chen".to_string(),
            date: "2026-06-26T00:00:00Z".to_string(),
            message: "seed".to_string(),
            changed_paths: vec![SvnLogPath {
                path: "/trunk/add-file.txt".to_string(),
                action: "A".to_string(),
            }],
        };

        assert!(!log_entry_matches(&entry, Some("add"), None));
        assert!(!log_entry_matches(&entry, Some("新增"), None));
        assert!(!log_entry_matches(&entry, Some("mark"), None));
    }

    #[test]
    fn log_date_range_scans_newest_first_and_includes_end_day() {
        let args = build_log_args(None, None, None, Some("2026-06-01"), Some("2026-06-26"));
        assert_eq!(args[4], "{2026-06-26T23:59:59.999999}:{2026-06-01}");
        let next = build_log_args(
            Some(50),
            Some(99),
            None,
            Some("2026-06-01"),
            Some("2026-06-26"),
        );
        assert_eq!(next[6], "99:{2026-06-01}");
        let end_only = build_log_args(None, None, None, None, Some("2026-06-26"));
        assert_eq!(end_only[4], "{2026-06-26T23:59:59.999999}:1");
    }

    #[test]
    fn log_date_filter_excludes_revision_before_start_day() {
        let entry = SvnLogEntry {
            revision: 8,
            author: "author".to_string(),
            date: "2026-06-25T12:00:00Z".to_string(),
            message: "earlier".to_string(),
            changed_paths: Vec::new(),
        };
        assert!(!log_entry_matches_dates(&entry, Some("2026-06-26"), None));
        assert!(log_entry_matches_dates(&entry, None, Some("2026-06-26")));
    }

    #[test]
    fn log_keyword_matches_commit_message() {
        let entry = SvnLogEntry {
            revision: 4,
            author: "mark_chen".to_string(),
            date: "2026-04-12T00:00:00Z".to_string(),
            message: "conf: add api key".to_string(),
            changed_paths: Vec::new(),
        };

        assert!(log_entry_matches(&entry, Some("add"), None));
    }

    #[test]
    fn log_author_filter_matches_exact_author_case_insensitively() {
        let entry = SvnLogEntry {
            revision: 8,
            author: "mark_chen".to_string(),
            date: "2026-06-26T00:00:00Z".to_string(),
            message: "seed".to_string(),
            changed_paths: Vec::new(),
        };

        assert!(log_entry_matches(&entry, None, Some("MARK_CHEN")));
        assert!(!log_entry_matches(&entry, None, Some("other")));
    }

    #[test]
    fn filtered_log_discards_duplicate_revisions() {
        let entry = SvnLogEntry {
            revision: 8,
            author: "mark_chen".to_string(),
            date: "2026-06-26T00:00:00Z".to_string(),
            message: "seed".to_string(),
            changed_paths: Vec::new(),
        };
        let batch = vec![entry.clone(), entry];
        let mut matches = Vec::new();
        let mut seen_revisions = HashSet::new();

        let reached_limit = append_unique_log_matches(
            &mut matches,
            &mut seen_revisions,
            &batch,
            None,
            Some("mark_chen"),
            None,
        );

        assert!(!reached_limit);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].revision, 8);
    }

    /// The other real-world shape: the repository holds a single spelling and the
    /// on-disk name drifted, so SVN reports the twin as *unversioned*. That must
    /// stay a local rename; classifying it as a repository duplicate would delete
    /// the only versioned entry.
    #[test]
    fn a_local_drift_is_not_mistaken_for_a_repository_duplicate() {
        use std::process::Command;
        if Command::new("svnadmin").arg("--version").output().is_err() {
            eprintln!("Skipping drift test: svnadmin is unavailable");
            return;
        }
        let directory = std::env::temp_dir().join(format!(
            "orcasvn-drift-test-{}-{}",
            std::process::id(),
            crate::test_support::unique_temp_suffix()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let repository = directory.join("repository");
        assert!(Command::new("svnadmin")
            .arg("create")
            .arg(&repository)
            .output()
            .unwrap()
            .status
            .success());
        let url = format!(
            "file:///{}",
            repository
                .to_string_lossy()
                .replace('\\', "/")
                .trim_start_matches('/')
        );
        let wc = directory.join("wc");
        let run = |args: &[&str], cwd: &Path| {
            let output = Command::new("svn")
                .args(args)
                .current_dir(cwd)
                .output()
                .unwrap();
            assert!(output.status.success(), "{args:?}");
        };
        run(&["checkout", "-q", &url, wc.to_str().unwrap()], &directory);
        let nfc = "カスタマイズ.xlsx";
        let nfd = "カスタマイス\u{3099}.xlsx";
        std::fs::create_dir(wc.join("d")).unwrap();
        std::fs::write(wc.join("d").join(nfc), "content").unwrap();
        run(&["add", "-q", "d"], &wc);
        run(&["commit", "-q", "-m", "add"], &wc);
        // Only one spelling exists in the repository; the disk name drifts.
        std::fs::rename(wc.join("d").join(nfc), wc.join("d").join(nfd)).unwrap();

        let wc_path = wc.to_str().unwrap().to_string();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut reported = runtime.block_on(status(&wc_path)).unwrap();
        assert_eq!(reported.len(), 1, "the pair collapses to one entry");
        assert_eq!(reported[0].status_code, "name_mismatch");
        runtime
            .block_on(super::classify_name_duplicates(&wc, &mut reported))
            .unwrap();
        assert_eq!(
            reported[0].status_code, "name_mismatch",
            "an unversioned twin is a local drift, never a repository duplicate"
        );

        // Renaming is the right fix and leaves a clean working copy.
        let repaired = runtime
            .block_on(super::repair_name_mismatches(
                &wc_path,
                &[format!("d/{nfc}")],
            ))
            .unwrap();
        assert_eq!(repaired, 1);
        assert!(runtime.block_on(status(&wc_path)).unwrap().is_empty());

        std::fs::remove_dir_all(&directory).unwrap();
    }

    /// macOS compares file names after Unicode normalisation, so an NFC entry in
    /// the repository matches an NFD name on disk. ext4 and NTFS keep the two
    /// spellings apart, which makes the duplicate and mismatch scenarios below
    /// impossible to reproduce there — their assertions would be wrong, not the
    /// code. This reports whether the filesystem folds the spellings together.
    fn filesystem_folds_normalization(directory: &Path) -> bool {
        let probe = directory.join("probe-normalization");
        if std::fs::create_dir_all(&probe).is_err() {
            return false;
        }
        let nfc = probe.join("カスタマイズ.txt");
        let nfd = probe.join("カスタマイス\u{3099}.txt");
        if std::fs::write(&nfc, b"probe").is_err() {
            let _ = std::fs::remove_dir_all(&probe);
            return false;
        }
        let folds = nfd.exists();
        let _ = std::fs::remove_dir_all(&probe);
        folds
    }

    /// The real-world case: the repository itself holds one file under two
    /// spellings. macOS materializes only one, so a fresh checkout always leaves
    /// a phantom `missing` entry. Cleanup must be a repository deletion, and a
    /// later checkout must be clean with no prompt at all.
    #[test]
    fn a_repository_duplicate_is_detected_deleted_and_leaves_clean_checkouts() {
        use std::process::Command;
        for tool in ["svnadmin", "svnmucc"] {
            if Command::new(tool).arg("--version").output().is_err() {
                eprintln!("Skipping duplicate-name test: {tool} is unavailable");
                return;
            }
        }
        let directory = std::env::temp_dir().join(format!(
            "orcasvn-dup-test-{}-{}",
            std::process::id(),
            crate::test_support::unique_temp_suffix()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        if !filesystem_folds_normalization(&directory) {
            eprintln!("Skipping duplicate-name test: this filesystem keeps NFC and NFD apart");
            std::fs::remove_dir_all(&directory).unwrap();
            return;
        }
        let repository = directory.join("repository");
        assert!(Command::new("svnadmin")
            .arg("create")
            .arg(&repository)
            .output()
            .unwrap()
            .status
            .success());
        let url = format!(
            "file:///{}",
            repository
                .to_string_lossy()
                .replace('\\', "/")
                .trim_start_matches('/')
        );
        let blob = directory.join("blob.txt");
        std::fs::write(&blob, "same content\n").unwrap();
        let nfc = "d/カスタマイズ.xlsx";
        let nfd = "d/カスタマイス\u{3099}.xlsx";
        // svnmucc writes both spellings straight into the repository; a working
        // copy on macOS cannot hold them both.
        let committed = Command::new("svnmucc")
            .args(["-U", &url, "-m", "add both spellings", "mkdir", "d"])
            .arg("put")
            .arg(&blob)
            .arg(nfc)
            .arg("put")
            .arg(&blob)
            .arg(nfd)
            .output()
            .unwrap();
        assert!(
            committed.status.success(),
            "{}",
            String::from_utf8_lossy(&committed.stderr)
        );

        let wc = directory.join("wc");
        let checkout = |target: &Path| {
            let output = Command::new("svn")
                .args(["checkout", "-q", &url, target.to_str().unwrap()])
                .output()
                .unwrap();
            assert!(output.status.success());
        };
        checkout(&wc);
        let wc_path = wc.to_str().unwrap().to_string();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut reported = runtime.block_on(status(&wc_path)).unwrap();
        assert_eq!(reported.len(), 1, "one phantom entry after checkout");
        assert_eq!(reported[0].status_code, "name_mismatch");
        runtime
            .block_on(super::classify_name_duplicates(&wc, &mut reported))
            .unwrap();
        assert_eq!(
            reported[0].status_code, "name_duplicate",
            "the repository versions both spellings, so a rename cannot help"
        );

        // Deleting the phantom keeps the file that is really on disk.
        runtime
            .block_on(super::delete_duplicate_names(
                &wc_path,
                &[reported[0].path.clone()],
            ))
            .unwrap();
        let scheduled = runtime.block_on(status(&wc_path)).unwrap();
        assert_eq!(scheduled.len(), 1);
        assert_eq!(scheduled[0].status_code, "deleted");
        let kept: Vec<String> = std::fs::read_dir(wc.join("d"))
            .unwrap()
            .map(|item| item.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            kept,
            vec!["カスタマイス\u{3099}.xlsx".to_string()],
            "the on-disk file is kept"
        );

        runtime
            .block_on(commit(
                &wc_path,
                "remove duplicate normalization entry",
                Some(&[reported[0].path.clone()]),
            ))
            .unwrap();

        // A brand-new checkout is clean: no phantom, nothing to repair.
        let fresh = directory.join("fresh");
        checkout(&fresh);
        let fresh_path = fresh.to_str().unwrap().to_string();
        let after_cleanup = runtime.block_on(status(&fresh_path)).unwrap();
        assert!(
            after_cleanup.is_empty(),
            "a checkout after the cleanup must not report anything, got {after_cleanup:?}"
        );
        assert_eq!(
            std::fs::read_dir(fresh.join("d")).unwrap().count(),
            1,
            "exactly one file remains"
        );

        std::fs::remove_dir_all(&directory).unwrap();
    }

    /// Drives the real streaming checkout against a local repository: the
    /// interface receives each processed item while the command runs, and the
    /// finished report carries the revision.
    #[test]
    fn streaming_checkout_reports_items_and_the_final_revision() {
        use std::process::Command;
        if Command::new("svnadmin").arg("--version").output().is_err() {
            eprintln!("Skipping streaming checkout test: svnadmin is unavailable");
            return;
        }
        let directory = std::env::temp_dir().join(format!(
            "orcasvn-stream-test-{}-{}",
            std::process::id(),
            crate::test_support::unique_temp_suffix()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let repository = directory.join("repository");
        assert!(Command::new("svnadmin")
            .arg("create")
            .arg(&repository)
            .output()
            .unwrap()
            .status
            .success());
        let url = format!(
            "file:///{}",
            repository
                .to_string_lossy()
                .replace('\\', "/")
                .trim_start_matches('/')
        );
        // Seed the repository through a scratch working copy.
        let seed = directory.join("seed");
        let run = |args: &[&str], cwd: &Path| {
            let output = Command::new("svn")
                .args(args)
                .current_dir(cwd)
                .output()
                .unwrap();
            assert!(output.status.success(), "{args:?}");
        };
        run(
            &["checkout", "-q", &url, seed.to_str().unwrap()],
            &directory,
        );
        std::fs::create_dir_all(seed.join("sub")).unwrap();
        for name in ["sub/a.txt", "sub/b.txt", "root.txt"] {
            std::fs::write(seed.join(name), "content").unwrap();
        }
        run(&["add", "-q", "sub", "root.txt"], &seed);
        run(&["commit", "-q", "-m", "seed"], &seed);

        let target = directory.join("target");
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = seen.clone();
        let report = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(super::checkout_streaming(
                &url,
                target.to_str().unwrap(),
                None,
                move |kind, line| recorded.lock().unwrap().push((kind, line)),
            ))
            .unwrap();

        assert_eq!(report.attempts, 1, "a healthy checkout needs one attempt");
        assert!(
            report.revision.is_some(),
            "the revision is parsed: {report:?}"
        );
        assert!(target.join("sub/a.txt").exists());

        let lines = seen.lock().unwrap().clone();
        let added: Vec<_> = lines
            .iter()
            .filter(|(kind, line)| {
                *kind == super::StreamKind::Stdout
                    && super::collapse_marker(line) == Some("A")
                    && line.contains("a.txt")
            })
            .collect();
        assert!(
            !added.is_empty(),
            "expected an 'A path' line for each file, got {lines:?}"
        );
        std::fs::remove_dir_all(&directory).unwrap();
    }

    /// An unreachable repository is a retryable transport failure: all attempts
    /// run, each announces itself, and the last error is returned.
    #[test]
    fn a_transport_failure_runs_every_retry_attempt() {
        let target = std::env::temp_dir().join(format!(
            "orcasvn-retry-test-{}-{}",
            std::process::id(),
            crate::test_support::unique_temp_suffix()
        ));
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = seen.clone();
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(super::checkout_streaming(
                "http://127.0.0.1:9/definitely-not-a-repository",
                target.to_str().unwrap(),
                None,
                move |kind, line| recorded.lock().unwrap().push((kind, line)),
            ));
        assert!(result.is_err(), "an unreachable repository must fail");
        let lines = seen.lock().unwrap().clone();
        let retries = lines
            .iter()
            .filter(|(kind, line)| *kind == super::StreamKind::Status && line.contains("次尝试"))
            .count();
        assert_eq!(retries, 2, "two retries after the first attempt: {lines:?}");
        let _ = std::fs::remove_dir_all(&target);
    }

    /// Regression: the salvage pass must enumerate entries without changing the
    /// sticky depth, otherwise SVN deletes the subtree it just downloaded.
    /// A repository that versions a `.svn` directory cannot be checked out in
    /// full, but the rest of the tree must still arrive and the reserved entry
    /// must not be reported as a server failure.
    #[test]
    fn salvage_survives_repository_entries_with_reserved_names() {
        use std::process::Command;
        for tool in ["svnadmin", "svnmucc"] {
            if Command::new(tool).arg("--version").output().is_err() {
                eprintln!("Skipping reserved-name test: {tool} is unavailable");
                return;
            }
        }
        let directory = std::env::temp_dir().join(format!(
            "orcasvn-reserved-test-{}-{}",
            std::process::id(),
            crate::test_support::unique_temp_suffix()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let repository = directory.join("repository");
        assert!(Command::new("svnadmin")
            .arg("create")
            .arg(&repository)
            .output()
            .unwrap()
            .status
            .success());
        let url = format!(
            "file:///{}",
            repository
                .to_string_lossy()
                .replace('\\', "/")
                .trim_start_matches('/')
        );
        let blob = directory.join("blob.txt");
        std::fs::write(&blob, "content\n").unwrap();
        // `foo.svn` is a legal file name; the directory `.svn` is not.
        let committed = Command::new("svnmucc")
            .args(["-U", &url, "-m", "seed", "mkdir", "d"])
            .arg("put")
            .arg(&blob)
            .arg("d/foo.svn")
            .args(["mkdir", "e", "mkdir", "e/.svn", "mkdir", "f"])
            .output()
            .unwrap();
        assert!(
            committed.status.success(),
            "{}",
            String::from_utf8_lossy(&committed.stderr)
        );

        let target = directory.join("target");
        let report = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(super::checkout_salvage(
                &url,
                target.to_str().unwrap(),
                |_, _| {},
            ))
            .unwrap();

        // The reserved entry is skipped, not reported as a broken directory.
        assert!(
            !report.failed.iter().any(|path| path.contains(".svn")),
            "reserved names are skips, not failures: {:?}",
            report.failed
        );
        assert!(
            !report
                .missing_files
                .iter()
                .any(|path| path.contains(".svn")),
            "reserved names are not missing files: {:?}",
            report.missing_files
        );
        // Everything else was fetched.
        assert_eq!(
            std::fs::read_to_string(target.join("d/foo.svn")).unwrap(),
            "content\n"
        );
        assert!(target.join("f").is_dir());

        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn reserved_names_are_recognised_and_never_retried() {
        for name in [".svn", ".SVN", "_svn", "foo.svn", "foo.SVN", "bar_svn"] {
            assert!(super::is_reserved_name(name), "{name}");
        }
        for name in [".subversion", "svn", "my-svn-file.txt", "file.svnx"] {
            assert!(!super::is_reserved_name(name), "{name}");
        }

        let message = "svn: Skipping argument: E200025: '.svn' ends in a reserved name";
        assert!(super::is_reserved_name_error(message));
        // A reserved-name refusal must not burn retry attempts.
        assert!(!super::is_stream_failure(message));
        assert!(!super::is_server_storage_failure(message));
    }

    #[test]
    fn the_file_counter_walks_nested_directories_and_skips_svn_metadata() {
        let root = std::env::temp_dir().join(format!(
            "orcasvn-count-test-{}-{}",
            std::process::id(),
            crate::test_support::unique_temp_suffix()
        ));
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::create_dir_all(root.join(".svn")).unwrap();
        std::fs::write(root.join("top.txt"), "x").unwrap();
        std::fs::write(root.join("a/one.txt"), "x").unwrap();
        std::fs::write(root.join("a/b/two.txt"), "x").unwrap();
        std::fs::write(root.join(".svn/entries"), "x").unwrap();
        assert_eq!(super::count_files(&root), 3);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_listing_is_parsed_into_names_and_kinds() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<lists><list path="d">
<entry kind="dir"><name>2025年</name><commit revision="1"/></entry>
<entry kind="file"><name>説明.pdf</name><size>12</size></entry>
</list></lists>"#;
        let entries = super::parse_status_list_xml(xml);
        assert_eq!(
            entries,
            vec![
                ("2025年".to_string(), true),
                ("説明.pdf".to_string(), false),
            ]
        );
    }

    /// Resuming a checkout of a CJK path used to be refused: SVN records the
    /// percent-encoded URL, so a raw string comparison saw a different repository.
    /// A working copy left at `--depth immediates` (what the salvage pass
    /// creates) must be completed by the resume path, not just re-checked at
    /// that depth.
    #[test]
    fn resuming_completes_a_working_copy_left_at_immediate_depth() {
        use std::process::Command;
        if Command::new("svnadmin").arg("--version").output().is_err() {
            eprintln!("Skipping resume test: svnadmin is unavailable");
            return;
        }
        let directory = std::env::temp_dir().join(format!(
            "orcasvn-resume-test-{}-{}",
            std::process::id(),
            crate::test_support::unique_temp_suffix()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let repository = directory.join("repository");
        assert!(Command::new("svnadmin")
            .arg("create")
            .arg(&repository)
            .output()
            .unwrap()
            .status
            .success());
        let url = format!(
            "file:///{}",
            repository
                .to_string_lossy()
                .replace('\\', "/")
                .trim_start_matches('/')
        );
        let seed = directory.join("seed");
        let run = |args: &[&str], cwd: &Path| {
            let output = Command::new("svn")
                .args(args)
                .current_dir(cwd)
                .output()
                .unwrap();
            assert!(output.status.success(), "{args:?}");
        };
        run(
            &["checkout", "-q", &url, seed.to_str().unwrap()],
            &directory,
        );
        std::fs::create_dir_all(seed.join("a/b")).unwrap();
        std::fs::write(seed.join("a/b/deep.txt"), "deep").unwrap();
        std::fs::write(seed.join("a/shallow.txt"), "shallow").unwrap();
        run(&["add", "-q", "a"], &seed);
        run(&["commit", "-q", "-m", "seed"], &seed);

        // A shallow copy, as left behind by a cancelled salved checkout.
        let partial = directory.join("partial");
        run(
            &[
                "checkout",
                "-q",
                "--depth",
                "immediates",
                &url,
                partial.to_str().unwrap(),
            ],
            &directory,
        );
        assert!(!partial.join("a/b/deep.txt").exists());

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let report = runtime
            .block_on(super::resume_checkout(partial.to_str().unwrap(), |_, _| {}))
            .unwrap();
        assert_eq!(report.attempts, 1);
        assert!(report.revision.is_some());
        assert!(
            partial.join("a/b/deep.txt").exists(),
            "the resume must lift the working copy back to full depth"
        );

        // A directory that is not a working copy is refused, not clobbered.
        let empty = directory.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert!(runtime
            .block_on(super::resume_checkout(empty.to_str().unwrap(), |_, _| {}))
            .is_err());

        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn a_locked_working_copy_is_recognised_from_the_real_messages() {
        for message in [
            "svn: E155004: Run 'svn cleanup' to remove locks (type 'svn help cleanup' for details)",
            "svn: E155004: Working copy '/tmp/wc' locked.",
            "svn: E155004: '/tmp/wc' is already locked.",
            "svn: E155037: Previous operation has not finished; run 'cleanup' if it was interrupted",
            "svn: database is locked",
        ] {
            assert!(super::is_work_copy_locked(message), "{message}");
        }
        for message in [
            "svn: E175009: The XML response contains invalid XML",
            "svn: E170001: Authorization failed",
            "svn: E000005: Can't read file '/data/x': Input/output error",
        ] {
            assert!(!super::is_work_copy_locked(message), "{message}");
        }
    }

    #[test]
    fn urls_compare_equal_across_percent_encoding_and_case() {
        assert!(super::same_repository_url(
            "http://10.6.22.11/repos/opmng/02.bou/2.业务/1.受注/2026年",
            "http://10.6.22.11/repos/opmng/02.bou/2.%E4%B8%9A%E5%8A%A1/1.%E5%8F%97%E6%B3%A8/2026%E5%B9%B4",
        ));
        assert!(super::same_repository_url(
            "http://Host.Example/repo/",
            "http://host.example/repo"
        ));
        assert!(super::same_repository_url(
            "http://host/repo/a%20b",
            "http://host/repo/a b"
        ));
        // Different paths or repositories stay different.
        assert!(!super::same_repository_url(
            "http://host/repo/2025年",
            "http://host/repo/2026年"
        ));
        assert!(!super::same_repository_url(
            "http://host/one/repo",
            "http://host/two/repo"
        ));
        // A malformed escape is kept literally rather than panicking.
        assert_eq!(super::percent_decode("a%ZZb"), "a%ZZb");
        assert_eq!(super::percent_decode("%E4%B8%9A"), "业");
    }

    #[test]
    fn salvage_enumeration_never_lowers_the_persistent_depth() {
        let args = super::salvage_entry_args("some/dir");
        assert!(args.contains(&"--depth".to_string()));
        assert!(
            !args.contains(&"--set-depth".to_string()),
            "a sticky depth change would delete the subtree: {args:?}"
        );
        assert_eq!(args.last().map(String::as_str), Some("some/dir"));
        assert!(args.iter().any(|arg| arg == "--"));
    }

    #[test]
    fn a_server_storage_fault_is_not_treated_as_a_transport_failure() {
        let storage = "svn: E000005: Can't read file '/data/svn/repos/opmng/db/revs/3/3434': Input/output error";
        assert!(super::is_server_storage_failure(storage));
        // Retrying cannot repair a damaged revision file, so it must not loop.
        assert!(!super::is_stream_failure(storage));
        assert!(!super::is_server_storage_failure(
            "svn: E175009: The XML response contains invalid XML"
        ));
    }

    /// The salvage pass fetches a healthy repository completely and reports no
    /// broken directory.
    #[test]
    fn salvage_checkout_fills_a_healthy_repository() {
        use std::process::Command;
        if Command::new("svnadmin").arg("--version").output().is_err() {
            eprintln!("Skipping salvage test: svnadmin is unavailable");
            return;
        }
        let directory = std::env::temp_dir().join(format!(
            "orcasvn-salvage-test-{}-{}",
            std::process::id(),
            crate::test_support::unique_temp_suffix()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let repository = directory.join("repository");
        assert!(Command::new("svnadmin")
            .arg("create")
            .arg(&repository)
            .output()
            .unwrap()
            .status
            .success());
        let url = format!(
            "file:///{}",
            repository
                .to_string_lossy()
                .replace('\\', "/")
                .trim_start_matches('/')
        );
        let seed = directory.join("seed");
        let run = |args: &[&str], cwd: &Path| {
            let output = Command::new("svn")
                .args(args)
                .current_dir(cwd)
                .output()
                .unwrap();
            assert!(output.status.success(), "{args:?}");
        };
        run(
            &["checkout", "-q", &url, seed.to_str().unwrap()],
            &directory,
        );
        std::fs::create_dir_all(seed.join("a/b")).unwrap();
        std::fs::write(seed.join("a/b/deep.txt"), "deep").unwrap();
        std::fs::write(seed.join("a/top.txt"), "top").unwrap();
        run(&["add", "-q", "a"], &seed);
        run(&["commit", "-q", "-m", "seed"], &seed);

        let target = directory.join("target");
        let report = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(super::checkout_salvage(
                &url,
                target.to_str().unwrap(),
                |_, _| {},
            ))
            .unwrap();
        assert!(report.failed.is_empty(), "a healthy repo has no failures");
        assert!(target.join("a/top.txt").exists());
        assert!(target.join("a/b/deep.txt").exists());

        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn the_retry_plan_switches_large_transfers_to_per_file_mode() {
        // A healthy repository is tried with bulk updates first (fastest).
        let plan = super::checkout_plan(false);
        assert_eq!(plan.len(), 3);
        assert!(plan[0].1.is_empty(), "first attempt uses server defaults");
        assert_eq!(
            plan[1].1,
            [("servers:global:http-bulk-updates", "no")],
            "the fix for a truncated report is the per-file mode"
        );
        assert!(plan[2]
            .1
            .iter()
            .any(|(key, value)| *key == "servers:global:http-compression" && *value == "no"));
        assert!(
            plan[1].0 && plan[2].0,
            "retries clean the partial checkout first"
        );

        // Once a host truncated a response, its next checkout starts in that mode.
        let skip = super::checkout_plan(true);
        assert_eq!(skip.len(), 2);
        assert_eq!(skip[0].1, [("servers:global:http-bulk-updates", "no")]);
    }

    #[test]
    fn only_http_hosts_remember_bulk_truncation() {
        assert_eq!(
            super::url_authority("http://10.6.22.11/repos/opmng"),
            Some("http://10.6.22.11".to_string())
        );
        assert_eq!(
            super::url_authority("https://Host.Example:8443/repo"),
            Some("https://host.example:8443".to_string())
        );
        // Local and svn:// transports have no bulk update mode to disable.
        assert_eq!(super::url_authority("file:///tmp/repo"), None);
        assert_eq!(super::url_authority("svn://host/repo"), None);

        super::remember_bulk_truncation("http://remember-test.local/repo");
        assert!(super::bulk_updates_unsafe(
            "http://remember-test.local/repo"
        ));
        super::forget_bulk_truncation("http://remember-test.local/repo");
        assert!(!super::bulk_updates_unsafe(
            "http://remember-test.local/repo"
        ));
        // Other hosts are unaffected.
        assert!(!super::bulk_updates_unsafe("http://another.local/repo"));
    }

    #[test]
    fn truncated_responses_are_retried_but_real_errors_are_not() {
        // The report seen on large checkouts through a proxy.
        let truncated = "svn: E175009: The XML response contains invalid XML\n\
svn: E130003: Malformed XML: no element found at line 41";
        assert!(super::is_stream_failure(truncated));
        assert!(super::is_stream_failure(
            "svn: E120171: Error running context"
        ));
        assert!(super::is_stream_failure("svn: E170013: Unable to connect"));
        // A genuine repository refusal must be reported at once.
        assert!(!super::is_stream_failure(
            "svn: E170001: Authorization failed for 'http://host/repo'"
        ));
        assert!(!super::is_stream_failure(
            "svn: E160013: URL 'http://host/repo/x' doesn't exist"
        ));
        assert!(!super::is_stream_failure("svn: E155004: locked"));
    }

    #[test]
    fn the_checked_out_revision_is_read_from_the_output() {
        assert_eq!(
            super::checked_out_revision("A    file\nChecked out revision 7394."),
            Some(7394)
        );
        assert_eq!(
            super::checked_out_revision("Checked out revision 12.\n"),
            Some(12)
        );
        assert_eq!(super::checked_out_revision("nothing here"), None);
    }

    #[test]
    fn nfc_and_nfd_names_of_one_file_collapse_into_a_name_mismatch() {
        let nfc = "完了ーポイント管理システム.pptx"; // U+30DD
        let nfd = "完了ー\u{30db}\u{309a}イント管理システム.pptx"; // ホ + combining dakuten
        let entry = |path: &str, code: &str| SvnStatus {
            path: path.to_string(),
            status: code.to_string(),
            status_code: code.to_string(),
            prop_status: "none".to_string(),
            locked: false,
            history: false,
            switched: false,
        };
        let collapsed = super::collapse_name_mismatches(
            Path::new("/nonexistent-workspace"),
            vec![
                entry(nfc, "missing"),
                entry(nfd, "unversioned"),
                entry("other/keep.txt", "modified"),
                entry("独自のファイル.txt", "missing"),
            ],
        );
        assert_eq!(collapsed.len(), 3, "the unversioned twin is dropped");
        assert!(collapsed
            .iter()
            .any(|item| item.path == nfc && item.status_code == "name_mismatch"));
        assert!(!collapsed.iter().any(|item| item.path == nfd));
        // A missing entry without a canonically equal sibling stays missing.
        assert!(collapsed
            .iter()
            .any(|item| item.path == "独自のファイル.txt" && item.status_code == "missing"));
        assert!(collapsed
            .iter()
            .any(|item| item.path == "other/keep.txt" && item.status_code == "modified"));

        // Running it twice must not change the result.
        assert_eq!(
            super::collapse_name_mismatches(Path::new("/nonexistent-workspace"), collapsed.clone())
                .len(),
            collapsed.len()
        );
    }

    /// The case seen in real working copies: `svn status` reports only `missing`
    /// (no unversioned twin) while a canonically equal name sits in the same
    /// directory. Detection must look at the filesystem, and repair must rename
    /// the entry to the repository spelling.
    #[test]
    fn a_lone_missing_entry_with_an_nfd_twin_on_disk_is_detected_and_repaired() {
        let root = std::env::temp_dir().join(format!(
            "orcasvn-nfd-probe-{}-{}",
            std::process::id(),
            crate::test_support::unique_temp_suffix()
        ));
        std::fs::create_dir_all(root.join("P2026.026")).unwrap();
        let nfc = "カスタマイズ.xlsx";
        let nfd = "カスタマイス\u{3099}.xlsx";
        std::fs::write(root.join("P2026.026").join(nfd), "x").unwrap();
        // A genuinely absent file must keep its `missing` status.
        let absences = "P2026.026/存在しない.xlsx";

        let entry = |path: &str| SvnStatus {
            path: path.to_string(),
            status: "missing".to_string(),
            status_code: "missing".to_string(),
            prop_status: "none".to_string(),
            locked: false,
            history: false,
            switched: false,
        };
        let collapsed = super::collapse_name_mismatches(
            &root,
            vec![entry(&format!("P2026.026/{nfc}")), entry(absences)],
        );
        assert_eq!(collapsed.len(), 2);
        assert!(collapsed
            .iter()
            .any(|item| item.status_code == "name_mismatch" && item.path.ends_with(nfc)));
        assert!(collapsed
            .iter()
            .any(|item| item.status_code == "missing" && item.path == absences));

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let repaired = runtime
            .block_on(super::repair_name_mismatches(
                root.to_str().unwrap(),
                &[format!("P2026.026/{nfc}")],
            ))
            .unwrap();
        assert_eq!(repaired, 1);
        let names: Vec<String> = std::fs::read_dir(root.join("P2026.026"))
            .unwrap()
            .flatten()
            .map(|item| item.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec![nfc.to_string()],
            "the NFD twin became the NFC name"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Reproduces the macOS case from a real working copy: the repository stores
    /// the NFC name while the filesystem holds the NFD one, so `svn status`
    /// reports one file as both unversioned and missing. Repairing the on-disk
    /// name must leave a clean working copy.
    #[test]
    fn name_mismatch_detection_and_repair_work_against_a_real_working_copy() {
        use std::process::Command;
        if Command::new("svnadmin").arg("--version").output().is_err() {
            eprintln!("Skipping local repository test: svnadmin is unavailable");
            return;
        }
        let directory = std::env::temp_dir().join(format!(
            "orcasvn-nfc-test-{}-{}",
            std::process::id(),
            crate::test_support::unique_temp_suffix()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        if !filesystem_folds_normalization(&directory) {
            eprintln!("Skipping name-mismatch test: this filesystem keeps NFC and NFD apart");
            std::fs::remove_dir_all(&directory).unwrap();
            return;
        }
        let repository = directory.join("repository");
        assert!(Command::new("svnadmin")
            .arg("create")
            .arg(&repository)
            .output()
            .unwrap()
            .status
            .success());
        let url = format!(
            "file:///{}",
            repository
                .to_string_lossy()
                .replace('\\', "/")
                .trim_start_matches('/')
        );
        let wc = directory.join("wc");
        let run = |args: &[&str], cwd: &Path| {
            let output = Command::new("svn")
                .args(args)
                .current_dir(cwd)
                .output()
                .unwrap();
            assert!(output.status.success(), "{args:?}");
        };
        run(&["checkout", "-q", &url, wc.to_str().unwrap()], &directory);

        let nfc = "完了ーポイント管理システム.pptx";
        let nfd = "完了ー\u{30db}\u{309a}イント管理システム.pptx";
        std::fs::write(wc.join(nfc), "content").unwrap();
        run(&["add", "-q", nfc], &wc);
        run(&["commit", "-q", "-m", "add"], &wc);
        let wc_path = wc.to_str().unwrap().to_string();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert!(runtime.block_on(status(&wc_path)).unwrap().is_empty());

        // The filesystem now holds a canonically equal but byte-different name.
        std::fs::rename(wc.join(nfc), wc.join(nfd)).unwrap();
        let reported = runtime.block_on(status(&wc_path)).unwrap();
        assert_eq!(
            reported
                .iter()
                .filter(|entry| entry.status_code == "name_mismatch")
                .count(),
            1,
            "expected a single name_mismatch entry, got {reported:?}"
        );
        assert!(!reported
            .iter()
            .any(|entry| entry.status_code == "unversioned"));

        let repaired = runtime
            .block_on(super::repair_name_mismatches(&wc_path, &[nfc.to_string()]))
            .unwrap();
        assert_eq!(repaired, 1);
        assert!(wc.join(nfc).exists());
        assert!(
            runtime.block_on(status(&wc_path)).unwrap().is_empty(),
            "the working copy must be clean after the rename"
        );

        std::fs::remove_dir_all(&directory).unwrap();
    }

    /// The workspace actions for missing and conflicted files call exactly these
    /// two commands, so their SVN semantics are pinned down against a real
    /// repository rather than a mock.
    #[test]
    fn missing_files_can_be_scheduled_for_deletion_and_conflicts_resolved() {
        use std::process::Command;
        if Command::new("svnadmin").arg("--version").output().is_err() {
            eprintln!("Skipping local repository test: svnadmin is unavailable");
            return;
        }
        let directory = std::env::temp_dir().join(format!(
            "orcasvn-resolve-test-{}-{}",
            std::process::id(),
            crate::test_support::unique_temp_suffix()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let repository = directory.join("repository");
        assert!(Command::new("svnadmin")
            .arg("create")
            .arg(&repository)
            .output()
            .unwrap()
            .status
            .success());
        let url = format!(
            "file:///{}",
            repository
                .to_string_lossy()
                .replace('\\', "/")
                .trim_start_matches('/')
        );
        let wc = directory.join("wc");
        let peer = directory.join("peer");
        let run = |args: &[&str], cwd: &Path| {
            let output = Command::new("svn")
                .args(args)
                .current_dir(cwd)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).into_owned()
        };
        run(&["checkout", "-q", &url, wc.to_str().unwrap()], &directory);
        for name in ["gone.txt", "conflict.txt"] {
            std::fs::write(wc.join(name), "base\n").unwrap();
        }
        run(&["add", "-q", "gone.txt", "conflict.txt"], &wc);
        run(&["commit", "-q", "-m", "initial"], &wc);
        run(
            &["checkout", "-q", &url, peer.to_str().unwrap()],
            &directory,
        );

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let wc_path = wc.to_str().unwrap().to_string();

        // A versioned file removed from disk is `missing`; scheduling its
        // deletion needs no --force and must not be rejected.
        std::fs::remove_file(wc.join("gone.txt")).unwrap();
        let missing = runtime.block_on(status(&wc_path)).unwrap();
        assert!(missing
            .iter()
            .any(|entry| { entry.path == "gone.txt" && entry.status_code == "missing" }));
        runtime
            .block_on(delete(&wc_path, &["gone.txt".to_string()]))
            .unwrap();
        let scheduled = runtime.block_on(status(&wc_path)).unwrap();
        assert!(scheduled
            .iter()
            .any(|entry| entry.path == "gone.txt" && entry.status_code == "deleted"));

        // A conflicting edit is resolved by adopting the repository version.
        std::fs::write(wc.join("conflict.txt"), "mine\n").unwrap();
        std::fs::write(peer.join("conflict.txt"), "theirs\n").unwrap();
        run(&["commit", "-q", "-m", "theirs"], &peer);
        runtime
            .block_on(update(&wc_path, None))
            .expect("update applies the incoming change");
        let conflicted = runtime.block_on(status(&wc_path)).unwrap();
        assert!(conflicted
            .iter()
            .any(|entry| { entry.path == "conflict.txt" && entry.status_code == "conflicted" }));
        runtime
            .block_on(resolve(
                &wc_path,
                &["conflict.txt".to_string()],
                "theirs-full",
            ))
            .unwrap();
        let resolved = runtime.block_on(status(&wc_path)).unwrap();
        assert!(
            !resolved
                .iter()
                .any(|entry| entry.path == "conflict.txt" && entry.status_code == "conflicted"),
            "the file must no longer be conflicted: {resolved:?}"
        );
        assert_eq!(
            std::fs::read_to_string(wc.join("conflict.txt")).unwrap(),
            "theirs\n",
            "theirs-full adopts the repository content"
        );

        std::fs::remove_dir_all(&directory).unwrap();
    }
}
