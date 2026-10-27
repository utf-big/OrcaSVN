//! Coordination between background status queries and mutating SVN commands.
//!
//! SVN protects its own working copy. A status query that overlaps a commit,
//! update or revert surfaces as `E155004 working copy locked` even though the
//! interface serialized the buttons. Mutating commands therefore hold an
//! exclusive gate per working copy, while status queries only *try* to read it:
//! when a mutation is in flight they reuse the last known entries instead of
//! blocking the interface for the length of a network transfer.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::{OwnedRwLockReadGuard, RwLock};
use tokio::task::AbortHandle;

use super::executor::SvnError;

type Gate = Arc<RwLock<()>>;

static GATES: OnceLock<Mutex<HashMap<PathBuf, Gate>>> = OnceLock::new();
static RUNNING: OnceLock<Mutex<HashMap<PathBuf, (u64, AbortHandle)>>> = OnceLock::new();
static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

fn gates() -> &'static Mutex<HashMap<PathBuf, Gate>> {
    GATES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn running() -> &'static Mutex<HashMap<PathBuf, (u64, AbortHandle)>> {
    RUNNING.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Gates and cancellation are keyed by the working copy as SVN sees it. A path
/// that cannot be resolved stays a valid key: the SVN command itself reports
/// the real error, and calls for the same string still share one gate.
fn key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn gate(path: &Path) -> Gate {
    let mut gates = gates().lock().unwrap_or_else(|error| error.into_inner());
    gates.entry(key(path)).or_default().clone()
}

/// Runs a mutating command while holding the exclusive gate for its working
/// copy, so two writes can never overlap and no status query runs meanwhile.
pub async fn exclusive<T, F>(path: &Path, operation: F) -> Result<T, SvnError>
where
    F: Future<Output = Result<T, SvnError>>,
{
    let gate = gate(path);
    let _guard = gate.write_owned().await;
    operation.await
}

/// Non-blocking read guard for status queries. `None` means a mutation holds
/// or queued for the gate, and the caller must not start an SVN query.
pub fn try_status_reader(path: &Path) -> Option<OwnedRwLockReadGuard<()>> {
    gate(path).try_read_owned().ok()
}

/// Read guard used when no previous status exists to fall back on: correctness
/// wins over responsiveness for the first scan of a workspace.
pub async fn status_reader(path: &Path) -> OwnedRwLockReadGuard<()> {
    gate(path).read_owned().await
}

/// Runs a long network command so `cancel` can abort it. Aborting drops the
/// future, and the executor's `kill_on_drop` then kills the SVN child instead of
/// leaving it running in the background.
pub async fn cancellable<T, F>(path: &Path, operation: F) -> Result<T, SvnError>
where
    T: Send + 'static,
    F: Future<Output = Result<T, SvnError>> + Send + 'static,
{
    let task = tokio::spawn(operation);
    let token = register_cancellation(path, task.abort_handle());
    let result = task.await;
    unregister_cancellation(path, token);
    match result {
        Ok(result) => result,
        Err(error) if error.is_cancelled() => Err(SvnError::Cancelled),
        Err(error) => Err(SvnError::CommandFailed(error.to_string())),
    }
}

/// Registers the running cancellable command for a working copy.
pub fn register_cancellation(path: &Path, handle: AbortHandle) -> u64 {
    let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
    let mut running = running().lock().unwrap_or_else(|error| error.into_inner());
    running.insert(key(path), (token, handle));
    token
}

/// Clears the registration only if it still belongs to this command, so a
/// finishing command cannot drop a newer one's handle.
pub fn unregister_cancellation(path: &Path, token: u64) {
    let mut running = running().lock().unwrap_or_else(|error| error.into_inner());
    if running
        .get(&key(path))
        .is_some_and(|(current, _)| *current == token)
    {
        running.remove(&key(path));
    }
}

/// Aborts the running cancellable command for this working copy. Returns
/// whether anything was cancelled; the command reports `SvnError::Cancelled`.
pub fn cancel(path: &Path) -> bool {
    let mut running = running().lock().unwrap_or_else(|error| error.into_inner());
    match running.remove(&key(path)) {
        Some((_, handle)) => {
            handle.abort();
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "orcasvn-gate-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    #[test]
    fn a_mutation_blocks_status_queries_while_it_runs() {
        runtime().block_on(async {
            let root = path();
            let writer = {
                let root = root.clone();
                tokio::spawn(async move {
                    exclusive(&root, async {
                        tokio::time::sleep(Duration::from_millis(40)).await;
                        Ok(())
                    })
                    .await
                    .unwrap();
                })
            };
            // Let the mutation reach the gate before probing it.
            let during = {
                let root = root.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    try_status_reader(&root).is_none()
                })
            };
            let afterwards = {
                let root = root.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(80)).await;
                    try_status_reader(&root).is_some()
                })
            };
            writer.await.unwrap();
            assert!(
                during.await.unwrap(),
                "status must not run during a mutation"
            );
            assert!(
                afterwards.await.unwrap(),
                "status must resume once the mutation ends"
            );
        });
    }

    #[test]
    fn concurrent_mutations_are_serialized_per_working_copy() {
        runtime().block_on(async {
            let root = path();
            let other = path();
            let depth = Arc::new(AtomicU64::new(0));
            let peak = Arc::new(AtomicU64::new(0));
            let mutate = |target: PathBuf, depth: Arc<AtomicU64>, peak: Arc<AtomicU64>| {
                tokio::spawn(async move {
                    exclusive(&target, async {
                        let current = depth.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(current, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        depth.fetch_sub(1, Ordering::SeqCst);
                        Ok(())
                    })
                    .await
                    .unwrap();
                })
            };
            let tasks = [
                mutate(root.clone(), depth.clone(), peak.clone()),
                mutate(root, depth.clone(), peak.clone()),
                mutate(other.clone(), depth.clone(), peak.clone()),
                mutate(other, depth, peak.clone()),
            ];
            for task in tasks {
                task.await.unwrap();
            }
            // Two distinct working copies may overlap, the same one may not.
            assert_eq!(peak.load(Ordering::SeqCst), 2);
        });
    }

    #[test]
    fn cancelling_aborts_the_registered_command_only_once() {
        runtime().block_on(async {
            let root = path();
            let task = tokio::spawn(async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok::<(), SvnError>(())
            });
            let token = register_cancellation(&root, task.abort_handle());
            assert!(cancel(&root));
            assert!(task.await.unwrap_err().is_cancelled());
            unregister_cancellation(&root, token);
            assert!(!cancel(&root), "a cancelled command is no longer running");
        });
    }

    #[test]
    fn a_finishing_command_cannot_drop_a_newer_registration() {
        runtime().block_on(async {
            let root = path();
            let first = tokio::spawn(async { std::future::pending::<()>().await });
            let second = tokio::spawn(async { std::future::pending::<()>().await });
            let stale = register_cancellation(&root, first.abort_handle());
            let current = register_cancellation(&root, second.abort_handle());
            unregister_cancellation(&root, stale);
            assert!(
                cancel(&root),
                "the newer registration must survive the older unregister"
            );
            assert!(second.await.unwrap_err().is_cancelled());
            unregister_cancellation(&root, current);
            first.abort();
        });
    }
}
