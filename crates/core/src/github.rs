use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;
use tokio::process::Command;

/// `CREATE_NO_WINDOW` process creation flag (Windows only).
/// Prevents a console window from flashing when a subprocess is spawned from
/// a GUI (windows_subsystem = "windows") application.
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const GH_API_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GhCounts {
    pub open_issues: u64,
    pub open_prs: u64,
}

#[derive(Debug, Error)]
pub enum GhError {
    #[error("gh CLI not found")]
    GhNotFound,
    #[error("gh not authenticated")]
    NotAuthenticated,
    #[error("not a GitHub remote")]
    NotGithubRemote,
    #[error("gh api error: {0}")]
    Api(String),
}

pub fn parse_owner_repo(remote_url: &str) -> Option<(String, String)> {
    let trimmed = remote_url.trim();
    let path = trimmed
        .strip_prefix("https://github.com/")
        .or_else(|| trimmed.strip_prefix("http://github.com/"))
        .or_else(|| trimmed.strip_prefix("git@github.com:"))
        .or_else(|| trimmed.strip_prefix("ssh://git@github.com/"))?;

    let mut parts = path.trim_end_matches('/').split('/');
    let owner = parts.next()?.trim();
    let repo = parts.next()?.trim().trim_end_matches(".git");
    if !is_valid_owner(owner) || !is_valid_repo(repo) || parts.next().is_some() {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

fn is_valid_owner(owner: &str) -> bool {
    !owner.is_empty()
        && owner.len() <= 39
        && !owner.starts_with('-')
        && !owner.ends_with('-')
        && owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn is_valid_repo(repo: &str) -> bool {
    !repo.is_empty()
        && repo.len() <= 100
        && repo
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Sync version of remote URL lookup using git2 (for use in blocking contexts).
///
/// Reads the `origin` remote URL from the repository without spawning a subprocess.
/// Returns `None` if the path is not a git repo or has no `origin` remote.
pub fn get_origin_url_sync(repo_path: &std::path::Path) -> Option<String> {
    let repo = git2::Repository::open(repo_path).ok()?;
    let remote = repo.find_remote("origin").ok()?;
    remote.url().ok().map(str::to_string)
}

/// High-level GitHub enrichment helper for one repository.
///
/// Calls [`fetch_counts_with_token_env`] internally and masks fields by
/// `cfg.enable_issues` / `cfg.enable_prs`. Returns `None` on any error
/// (including non-GitHub URLs); non-GitHub-remote errors are silent, others
/// are logged at `debug!`.
pub async fn fetch_counts_for_remote(
    remote_url: &str,
    cfg: &crate::config::GithubConfig,
) -> Option<crate::GithubInfo> {
    match fetch_counts_with_token_env(remote_url, &cfg.token_env).await {
        Ok(counts) => Some(crate::GithubInfo {
            open_issues: cfg.enable_issues.then_some(counts.open_issues),
            open_prs: cfg.enable_prs.then_some(counts.open_prs),
        }),
        Err(GhError::NotGithubRemote) => None,
        Err(e) => {
            tracing::debug!(error = %e, "fetch_counts_for_remote skipped");
            None
        }
    }
}

pub async fn fetch_counts(remote_url: &str) -> Result<GhCounts, GhError> {
    fetch_counts_with_token(remote_url, None).await
}

pub async fn fetch_counts_with_token_env(
    remote_url: &str,
    token_env: &str,
) -> Result<GhCounts, GhError> {
    let token = std::env::var(token_env).ok();
    fetch_counts_with_token(remote_url, token.as_deref()).await
}

async fn fetch_counts_with_token(
    remote_url: &str,
    token: Option<&str>,
) -> Result<GhCounts, GhError> {
    let (owner, name) = parse_owner_repo(remote_url).ok_or(GhError::NotGithubRemote)?;
    let query = r#"
query($owner: String!, $name: String!) {
  repository(owner: $owner, name: $name) {
    issues(states: OPEN) { totalCount }
    pullRequests(states: OPEN) { totalCount }
  }
}
"#;

    let mut command = Command::new("gh");
    command
        .arg("api")
        .arg("graphql")
        .arg("-f")
        .arg(format!("query={query}"))
        .arg("-f")
        .arg(format!("owner={owner}"))
        .arg("-f")
        .arg(format!("name={name}"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(target_os = "windows")]
    command.creation_flags(CREATE_NO_WINDOW);
    if let Some(token) = token {
        command.env("GH_TOKEN", token).env("GITHUB_TOKEN", token);
    }

    #[cfg(unix)]
    command.process_group(0);
    let child = command.kill_on_drop(true).spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            GhError::GhNotFound
        } else {
            GhError::Api(e.to_string())
        }
    })?;
    #[cfg(unix)]
    let _group = ProcessGroup(child.id());
    let output = tokio::time::timeout(GH_API_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| GhError::Api("gh api timed out".to_string()))?
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                GhError::GhNotFound
            } else {
                GhError::Api(e.to_string())
            }
        })?;

    if !output.status.success() {
        if output.status.code() == Some(4) {
            return Err(GhError::NotAuthenticated);
        }
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(GhError::Api(if stderr.is_empty() {
            format!("gh exited with {}", output.status)
        } else {
            stderr
        }));
    }

    parse_counts(&output.stdout)
}

fn parse_counts(bytes: &[u8]) -> Result<GhCounts, GhError> {
    #[derive(Deserialize)]
    struct Response {
        data: Data,
    }
    #[derive(Deserialize)]
    struct Data {
        repository: Repository,
    }
    #[derive(Deserialize)]
    struct Repository {
        issues: TotalCount,
        #[serde(rename = "pullRequests")]
        pull_requests: TotalCount,
    }
    #[derive(Deserialize)]
    struct TotalCount {
        #[serde(rename = "totalCount")]
        total_count: u64,
    }

    let response: Response =
        serde_json::from_slice(bytes).map_err(|e| GhError::Api(e.to_string()))?;
    Ok(GhCounts {
        open_issues: response.data.repository.issues.total_count,
        open_prs: response.data.repository.pull_requests.total_count,
    })
}

// gh may invoke a credential/browser helper. Killing only gh leaves that helper
// behind on cancellation; the dedicated group belongs exclusively to this call.
#[cfg(unix)]
struct ProcessGroup(Option<u32>);
#[cfg(unix)]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        unsafe extern "C" {
            fn kill(pid: i32, signal: i32) -> i32;
        }
        if let Some(pid) = self.0.and_then(|pid| i32::try_from(pid).ok()) {
            // SAFETY: spawned above in its own process group, never our group.
            unsafe {
                kill(-pid, 9);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_owner_repo_accepts_https() {
        assert_eq!(
            parse_owner_repo("https://github.com/owner/repo.git"),
            Some(("owner".into(), "repo".into()))
        );
        assert_eq!(
            parse_owner_repo("https://github.com/owner/repo"),
            Some(("owner".into(), "repo".into()))
        );
    }

    #[test]
    fn parse_owner_repo_accepts_scp_like_ssh() {
        assert_eq!(
            parse_owner_repo("git@github.com:owner/repo.git"),
            Some(("owner".into(), "repo".into()))
        );
    }

    #[test]
    fn parse_owner_repo_accepts_ssh_url() {
        assert_eq!(
            parse_owner_repo("ssh://git@github.com/owner/repo.git"),
            Some(("owner".into(), "repo".into()))
        );
    }

    #[test]
    fn parse_owner_repo_rejects_non_github() {
        assert_eq!(parse_owner_repo("https://example.com/owner/repo"), None);
        assert_eq!(parse_owner_repo("git@gitlab.com:owner/repo.git"), None);
    }

    #[test]
    fn parse_owner_repo_rejects_invalid_slugs() {
        assert_eq!(parse_owner_repo("https://github.com/-owner/repo"), None);
        assert_eq!(parse_owner_repo("https://github.com/owner/repo@bad"), None);
    }

    #[test]
    fn parse_counts_reads_graphql_response() {
        let json = br#"{
          "data": {
            "repository": {
              "issues": { "totalCount": 12 },
              "pullRequests": { "totalCount": 3 }
            }
          }
        }"#;
        let counts = parse_counts(json).expect("parse counts");
        assert_eq!(counts.open_issues, 12);
        assert_eq!(counts.open_prs, 3);
    }
    #[test]
    fn fetch_counts_for_remote_non_github_returns_none_sync() {
        // parse_owner_repo returns None for non-GitHub → NotGithubRemote → None
        // We test this via fetch_counts_for_remote via the underlying parse path.
        // The function is async so we test parse_owner_repo directly for the None case.
        assert_eq!(parse_owner_repo("https://gitlab.com/owner/repo"), None);
    }
}
