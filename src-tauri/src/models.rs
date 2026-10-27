//! Data types shared across the Tauri command boundary.
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct CommandResult {
    pub success: bool,
    pub output: String,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OpenTarget {
    Explorer,
    Vscode,
    Terminal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StashUnversionedEntry {
    pub path: String,
    pub kind: String,
    pub content_base64: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GitignoreData {
    pub content: String,
    pub mtime: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SvnStatus {
    pub path: String,
    pub status: String,
    pub status_code: String,
    pub prop_status: String,
    pub locked: bool,
    pub history: bool,
    pub switched: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SvnLogEntry {
    pub revision: u64,
    pub author: String,
    pub date: String,
    pub message: String,
    pub changed_paths: Vec<SvnLogPath>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SvnLogPath {
    pub path: String,
    pub action: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SvnInfo {
    pub path: String,
    pub url: String,
    pub repository_root: String,
    pub revision: u64,
    pub node_kind: String,
    pub schedule: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SvnAuthUser {
    pub username: String,
    pub realm: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DiffResult {
    pub path: String,
    pub diff: String,
    pub old_revision: u64,
    pub new_revision: u64,
}

/// Error returned across the command boundary.
///
/// `code` is stable and drives the explanation the interface shows; `message`
/// keeps the original SVN context so nothing is lost for diagnosis.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandError {
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl CommandError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            detail: None,
        }
    }

    /// Classifies the well-known SVN failures a user can act on, so the
    /// interface can suggest cleanup, credentials or conflict resolution
    /// instead of only echoing stderr.
    pub fn from_svn_output(message: String) -> Self {
        let code = classify_svn_failure(&message);
        Self {
            code,
            message,
            detail: None,
        }
    }
}

impl From<crate::svn::SvnError> for CommandError {
    fn from(error: crate::svn::SvnError) -> Self {
        use crate::svn::SvnError;
        match error {
            SvnError::CommandFailed(message) => Self::from_svn_output(message),
            SvnError::SvnNotFound => Self::new("svn_not_found", "SVN 未安装或不在 PATH 中"),
            SvnError::ParseError(message) => Self::new("svn_parse_failed", message),
            SvnError::InvalidArguments(message) => Self::new("svn_invalid_arguments", message),
            SvnError::Timeout => Self::new("svn_timeout", "SVN 操作超时"),
            SvnError::Cancelled => Self::new("svn_cancelled", "SVN 操作已取消"),
        }
    }
}

fn classify_svn_failure(message: &str) -> &'static str {
    let lowered = message.to_ascii_lowercase();
    let has = |needle: &str| lowered.contains(needle);
    if has("e155004")
        || has("e155037")
        || has("working copy locked")
        || has("is already locked")
        || has("database is locked")
    {
        "svn_locked"
    } else if has("e170001")
        || has("e215004")
        || has("authorization failed")
        || has("authentication failed")
    {
        "svn_auth"
    } else if has("e155015") || has("remains in conflict") {
        "svn_conflict"
    } else if has("e160013") || has("path not found") || has("e200009") {
        "svn_path_missing"
    } else if has("e000005")
        || has("input/output error")
        || has("can't read file")
        || has("cannot read file")
    {
        "svn_server_storage"
    } else if has("e175009") || has("e130003") || has("malformed xml") {
        "svn_response_truncated"
    } else if has("e120171")
        || has("e170013")
        || has("e175002")
        || has("e670002")
        || has("unable to connect")
        || has("could not resolve host")
    {
        "svn_network"
    } else {
        "svn_failed"
    }
}

#[cfg(test)]
mod tests {
    use super::{classify_svn_failure, CommandError};

    #[test]
    fn actionable_svn_failures_get_their_own_code() {
        let cases = [
            ("svn: E155004: Working copy '/wc' locked.", "svn_locked"),
            (
                "svn: E155037: Previous operation has not finished; run 'cleanup'",
                "svn_locked",
            ),
            (
                "svn: E170001: Authorization failed for 'https://example.test'",
                "svn_auth",
            ),
            ("svn: E155015: 'a.txt' remains in conflict", "svn_conflict"),
            (
                "svn: E160013: File not found: revision 7",
                "svn_path_missing",
            ),
            (
                "svn: E170013: Unable to connect to a repository at URL 'svn://h/r'",
                "svn_network",
            ),
            ("svn: E999999: Something else broke", "svn_failed"),
        ];
        for (message, expected) in cases {
            assert_eq!(classify_svn_failure(message), expected, "{message}");
        }
    }

    #[test]
    fn svn_errors_keep_their_message_and_map_to_a_code() {
        let error = CommandError::from(crate::svn::SvnError::Timeout);
        assert_eq!(error.code, "svn_timeout");
        assert_eq!(error.message, "SVN 操作超时");
        let error = CommandError::from(crate::svn::SvnError::CommandFailed(
            "svn: E155004: locked".to_string(),
        ));
        assert_eq!(error.code, "svn_locked");
        assert!(error.message.contains("E155004"));
    }
}

/// One stash record: a text patch plus the unversioned files it also saved.
/// Persisted as JSON under the app data directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StashRecord {
    pub id: String,
    pub name: String,
    pub workspace_path: String,
    pub created_at: String,
    pub patch: String,
    pub files: Vec<String>,
    pub hunk_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unversioned_files: Option<Vec<StashUnversionedEntry>>,
}
