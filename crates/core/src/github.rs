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

/// `(owner, name)` of a GitHub repository.
pub type RepoSlug = (String, String);

/// Upper bound on repositories per GraphQL request (keeps the query and
/// command line small and the per-request cost modest).
pub const GITHUB_BATCH_MAX: usize = 50;

/// Issue/PR counts for several repositories in one `gh api graphql` call.
///
/// Returns one entry per input slug, in order; fields are masked by
/// `cfg.enable_issues` / `cfg.enable_prs`. A repository GitHub cannot resolve
/// (deleted, renamed, no access) yields `None` without failing the others;
/// a failure of the whole request yields all `None` (logged at `debug!`).
pub async fn fetch_infos_batch(
    slugs: &[RepoSlug],
    cfg: &crate::config::GithubConfig,
) -> Vec<Option<crate::GithubInfo>> {
    let token = std::env::var(&cfg.token_env).ok();
    match fetch_counts_batch(slugs, token.as_deref()).await {
        Ok(counts) => counts
            .into_iter()
            .map(|c| {
                c.map(|c| crate::GithubInfo {
                    open_issues: cfg.enable_issues.then_some(c.open_issues),
                    open_prs: cfg.enable_prs.then_some(c.open_prs),
                })
            })
            .collect(),
        Err(e) => {
            tracing::debug!(error = %e, repos = slugs.len(), "GitHub batch lookup skipped");
            vec![None; slugs.len()]
        }
    }
}

/// GraphQL document with one aliased `repository` field per slug; owners and
/// names are passed as variables (`o0`/`n0`, …), never interpolated.
fn batch_query(count: usize) -> String {
    let vars = (0..count)
        .map(|i| format!("$o{i}: String!, $n{i}: String!"))
        .collect::<Vec<_>>()
        .join(", ");
    let fields = (0..count)
        .map(|i| {
            format!(
                "r{i}: repository(owner: $o{i}, name: $n{i}) {{ issues(states: OPEN) {{ totalCount }} pullRequests(states: OPEN) {{ totalCount }} }}"
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("query({vars}) {{\n{fields}\n}}")
}

async fn fetch_counts_batch(
    slugs: &[RepoSlug],
    token: Option<&str>,
) -> Result<Vec<Option<GhCounts>>, GhError> {
    if slugs.is_empty() {
        return Ok(Vec::new());
    }
    let mut command = Command::new("gh");
    command
        .arg("api")
        .arg("graphql")
        .arg("-f")
        .arg(format!("query={}", batch_query(slugs.len())));
    for (i, (owner, name)) in slugs.iter().enumerate() {
        command
            .arg("-f")
            .arg(format!("o{i}={owner}"))
            .arg("-f")
            .arg(format!("n{i}={name}"));
    }
    command
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

    if output.status.code() == Some(4) {
        return Err(GhError::NotAuthenticated);
    }
    // gh exits non-zero when any alias fails (e.g. a deleted repository) but
    // still prints the partial `data`, so parse stdout regardless of status.
    parse_batch_counts(&output.stdout, slugs.len()).map_err(|e| {
        if output.status.success() {
            e
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            GhError::Api(if stderr.is_empty() {
                format!("gh exited with {}", output.status)
            } else {
                stderr
            })
        }
    })
}

fn parse_batch_counts(bytes: &[u8], count: usize) -> Result<Vec<Option<GhCounts>>, GhError> {
    #[derive(Deserialize)]
    struct Response {
        data: Option<std::collections::HashMap<String, Option<Repository>>>,
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
    let mut data = response
        .data
        .ok_or_else(|| GhError::Api("GraphQL response has no data".to_string()))?;
    Ok((0..count)
        .map(|i| {
            data.remove(&format!("r{i}")).flatten().map(|r| GhCounts {
                open_issues: r.issues.total_count,
                open_prs: r.pull_requests.total_count,
            })
        })
        .collect())
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
    fn batch_query_uses_one_alias_and_variable_pair_per_repo() {
        let q = batch_query(2);
        assert!(q.starts_with("query($o0: String!, $n0: String!, $o1: String!, $n1: String!)"));
        assert!(q.contains("r0: repository(owner: $o0, name: $n0)"));
        assert!(q.contains("r1: repository(owner: $o1, name: $n1)"));
        assert!(!q.contains("r2:"));
    }

    #[test]
    fn parse_batch_counts_keeps_order_and_tolerates_unresolved_repos() {
        // Shape gh prints (exit 1) when one alias cannot be resolved.
        let json = br#"{
          "data": {
            "r1": null,
            "r0": { "issues": { "totalCount": 12 }, "pullRequests": { "totalCount": 3 } },
            "r2": { "issues": { "totalCount": 0 }, "pullRequests": { "totalCount": 1 } }
          },
          "errors": [{ "type": "NOT_FOUND", "path": ["r1"], "message": "Could not resolve" }]
        }"#;
        let counts = parse_batch_counts(json, 3).expect("parse counts");
        assert_eq!(
            counts,
            vec![
                Some(GhCounts {
                    open_issues: 12,
                    open_prs: 3
                }),
                None,
                Some(GhCounts {
                    open_issues: 0,
                    open_prs: 1
                }),
            ]
        );
    }

    #[test]
    fn parse_batch_counts_rejects_responses_without_data() {
        let json = br#"{ "errors": [{ "message": "Bad credentials" }] }"#;
        assert!(parse_batch_counts(json, 1).is_err());
        assert!(parse_batch_counts(b"not json", 1).is_err());
    }

    #[test]
    fn non_github_remotes_have_no_slug() {
        // Rows without a slug are never sent to GitHub.
        assert_eq!(parse_owner_repo("https://gitlab.com/owner/repo"), None);
    }
}
