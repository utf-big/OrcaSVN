//! Settings-related commands: SVN executable and ignore rules.
use crate::models::{CommandError, GitignoreData};
use crate::svn;
use std::fs;
use std::path::Path;

#[tauri::command]
pub(crate) async fn configure_svn_executable(
    executable: Option<String>,
) -> Result<String, CommandError> {
    svn::configure_svn_executable(executable.as_deref())
        .await
        .map_err(CommandError::from)
}

#[tauri::command]
pub(crate) async fn read_gitignore(path: String) -> Result<Option<GitignoreData>, CommandError> {
    let gitignore_path = Path::new(&path).join(".gitignore");
    if !gitignore_path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&gitignore_path)
        .map_err(|e| CommandError::new("io_error", e.to_string()))?;
    let mtime = fs::metadata(&gitignore_path)
        .map_err(|e| CommandError::new("io_error", e.to_string()))?
        .modified()
        .map_err(|e| CommandError::new("io_error", e.to_string()))?
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    Ok(Some(GitignoreData { content, mtime }))
}
