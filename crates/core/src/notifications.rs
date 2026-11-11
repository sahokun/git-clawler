use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::list::{ListRow, ScanOutcome};

/// Unique identifier for a monitored repo (its path as string).
pub type TargetId = String;

/// Minimal fingerprint of a repo's status for change detection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoFingerprint {
    pub dirty: bool,
    pub uncommitted_files: u32,
    pub unpushed_branches: u32,
    pub ahead: u32,
    pub behind: u32,
    pub conflict_predicted: Option<bool>,
}

/// Snapshot of all repos' fingerprints.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot(pub BTreeMap<TargetId, RepoFingerprint>);

impl Snapshot {
    pub fn new() -> Self {
        Self(BTreeMap::new())
    }

    pub fn from_rows(rows: &[ListRow]) -> Self {
        let fingerprints = rows
            .iter()
            .filter_map(|row| match &row.outcome {
                ScanOutcome::Ok(status) => Some((
                    row.target.path.to_string_lossy().into_owned(),
                    RepoFingerprint {
                        dirty: status.uncommitted_files > 0,
                        uncommitted_files: status.uncommitted_files,
                        unpushed_branches: status.unpushed_branches.len() as u32,
                        ahead: status.ahead,
                        behind: status.behind,
                        conflict_predicted: status.conflict_predicted,
                    },
                )),
                ScanOutcome::Err(_) => None,
            })
            .collect();
        Self(fingerprints)
    }
}

/// What changed between two snapshots.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ChangeSet {
    pub became_dirty: Vec<TargetId>,
    pub new_ahead_behind: Vec<TargetId>,
    pub new_conflicts: Vec<TargetId>,
}

impl ChangeSet {
    pub fn is_empty(&self) -> bool {
        self.became_dirty.is_empty()
            && self.new_ahead_behind.is_empty()
            && self.new_conflicts.is_empty()
    }

    /// Build a human-readable notification summary.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if !self.became_dirty.is_empty() {
            parts.push(format!(
                "{} repos have uncommitted changes",
                self.became_dirty.len()
            ));
        }
        if !self.new_ahead_behind.is_empty() {
            parts.push(format!(
                "{} repos have new ahead/behind changes",
                self.new_ahead_behind.len()
            ));
        }
        if !self.new_conflicts.is_empty() {
            parts.push(format!(
                "{} repos may have pull conflicts",
                self.new_conflicts.len()
            ));
        }
        if parts.is_empty() {
            "No repository changes".to_string()
        } else {
            parts.join("; ")
        }
    }
}

/// Diff two snapshots, returning what changed.
pub fn diff(prev: &Snapshot, next: &Snapshot) -> ChangeSet {
    let mut changes = ChangeSet::default();
    for (target, next_fp) in &next.0 {
        let Some(prev_fp) = prev.0.get(target) else {
            continue;
        };
        if next_fp.uncommitted_files > prev_fp.uncommitted_files {
            changes.became_dirty.push(target.clone());
        }
        if next_fp.ahead > prev_fp.ahead
            || next_fp.behind > prev_fp.behind
            || next_fp.unpushed_branches > prev_fp.unpushed_branches
        {
            changes.new_ahead_behind.push(target.clone());
        }
        if prev_fp.conflict_predicted != Some(true) && next_fp.conflict_predicted == Some(true) {
            changes.new_conflicts.push(target.clone());
        }
    }
    changes
}

/// Tracks global notification cooldown.
#[derive(Debug, Clone)]
pub struct CooldownTracker {
    last_global: Option<Instant>,
    cooldown: Duration,
}

impl CooldownTracker {
    pub fn new(cooldown_secs: u64) -> Self {
        Self {
            last_global: None,
            cooldown: Duration::from_secs(cooldown_secs),
        }
    }

    /// Returns true if enough time has passed since last notification.
    pub fn allow(&mut self) -> bool {
        self.last_global
            .map(|last| last.elapsed() >= self.cooldown)
            .unwrap_or(true)
    }

    pub fn record(&mut self) {
        self.last_global = Some(Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::list::{ListRow, ListTarget, ScanOutcome};
    use crate::RepoStatus;
    use chrono::Utc;
    use std::path::PathBuf;

    fn snapshot(path: &str, status: RepoStatus) -> Snapshot {
        Snapshot::from_rows(&[ListRow {
            target: ListTarget {
                index: 0,
                name: "repo".into(),
                path: PathBuf::from(path),
                is_current: false,
                distro: None,
                fetch: false,
            },
            scanned_at: Utc::now(),
            outcome: ScanOutcome::Ok(status),
            remote_url: None,
        }])
    }

    #[test]
    fn diff_detects_became_dirty() {
        let prev = snapshot("/repo", RepoStatus::default());
        let next = snapshot(
            "/repo",
            RepoStatus {
                uncommitted_files: 1,
                ..RepoStatus::default()
            },
        );
        assert_eq!(diff(&prev, &next).became_dirty, vec!["/repo"]);
    }

    #[test]
    fn diff_detects_uncommitted_count_increase() {
        let prev = snapshot(
            "/repo",
            RepoStatus {
                uncommitted_files: 1,
                ..RepoStatus::default()
            },
        );
        let next = snapshot(
            "/repo",
            RepoStatus {
                uncommitted_files: 2,
                ..RepoStatus::default()
            },
        );
        assert_eq!(diff(&prev, &next).became_dirty, vec!["/repo"]);
    }

    #[test]
    fn diff_detects_increased_ahead_or_behind() {
        let prev = snapshot(
            "/repo",
            RepoStatus {
                ahead: 1,
                behind: 0,
                ..RepoStatus::default()
            },
        );
        let next = snapshot(
            "/repo",
            RepoStatus {
                ahead: 1,
                behind: 2,
                ..RepoStatus::default()
            },
        );
        assert_eq!(diff(&prev, &next).new_ahead_behind, vec!["/repo"]);
    }

    #[test]
    fn diff_detects_new_conflict() {
        let prev = snapshot(
            "/repo",
            RepoStatus {
                conflict_predicted: None,
                ..RepoStatus::default()
            },
        );
        let next = snapshot(
            "/repo",
            RepoStatus {
                conflict_predicted: Some(true),
                ..RepoStatus::default()
            },
        );
        assert_eq!(diff(&prev, &next).new_conflicts, vec!["/repo"]);
    }

    #[test]
    fn summary_combines_change_types() {
        let changes = ChangeSet {
            became_dirty: vec!["a".into(), "b".into()],
            new_ahead_behind: vec!["c".into()],
            new_conflicts: vec![],
        };
        assert_eq!(
            changes.summary(),
            "2 repos have uncommitted changes; 1 repos have new ahead/behind changes"
        );
    }

    #[test]
    fn cooldown_allows_first_and_blocks_immediate_second() {
        let mut tracker = CooldownTracker::new(300);
        assert!(tracker.allow());
        tracker.record();
        assert!(!tracker.allow());
    }
}
