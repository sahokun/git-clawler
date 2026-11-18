//! Scan-root discovery against real repositories and linked worktrees.

use std::fs;
use std::path::{Path, PathBuf};

use git_clawler_core::config::{AppConfig, RepoEntry, ScanRootEntry};
use git_clawler_core::discovery::{
    apply, discover_native, discover_root, record_exclusion, worktree_gone, Found,
};
use git_clawler_core::list::{dedupe_worktree_branches, ListRow, ListTarget, ScanOutcome};
use git_clawler_core::model::{RepoStatus, UnpushedBranch};
use git_clawler_core::status::{scan, StatusError};

/// Same-directory key: resolves 8.3 short names/symlinks when the path
/// exists (Windows CI temp dirs use `RUNNER~1`), then normalises separators.
fn key(p: &Path) -> String {
    let resolved = fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let s = resolved.to_string_lossy().replace('\\', "/");
    let s = s
        .trim_start_matches("//?/")
        .trim_end_matches('/')
        .to_string();
    if cfg!(windows) {
        s.to_ascii_lowercase()
    } else {
        s
    }
}

fn init_repo(path: &Path) -> git2::Repository {
    fs::create_dir_all(path).unwrap();
    let repo = git2::Repository::init(path).unwrap();
    fs::write(path.join("README.md"), "x").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("README.md")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("t", "t@example.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
    }
    repo
}

fn add_worktree(repo: &git2::Repository, name: &str, path: &Path) {
    repo.worktree(name, path, None).unwrap();
}

fn refresh(config: &mut AppConfig) -> bool {
    let discovery = discover_native(config);
    apply(config, &discovery)
}

fn root(path: &Path) -> ScanRootEntry {
    ScanRootEntry {
        path: path.to_path_buf(),
        depth: 1,
        exclude: Vec::new(),
        distro: None,
        auto: true,
        worktrees: true,
    }
}

fn find<'a>(found: &'a [Found], path: &Path) -> Option<&'a Found> {
    found.iter().find(|f| key(&f.path) == key(path))
}

#[test]
fn discovers_children_sibling_and_external_worktrees() {
    let base = tempfile::tempdir().unwrap();
    let projects = base.path().join("projects");
    let outside = base.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    let app = init_repo(&projects.join("app"));
    add_worktree(&app, "sib", &projects.join("app-sib"));
    add_worktree(&app, "ext", &outside.join("app-ext"));
    init_repo(&projects.join("group").join("deep")); // depth 2: not found at depth 1

    let found = discover_root(&root(&projects)).unwrap();
    assert_eq!(found.len(), 3, "{found:?}");
    assert!(find(&found, &projects.join("app"))
        .unwrap()
        .worktree_of
        .is_none());
    for wt in [projects.join("app-sib"), outside.join("app-ext")] {
        let item = find(&found, &wt).unwrap_or_else(|| panic!("missing {wt:?} in {found:?}"));
        assert_eq!(
            key(item.worktree_of.as_ref().unwrap()),
            key(&projects.join("app"))
        );
    }
    assert!(find(&found, &projects.join("group").join("deep")).is_none());

    let mut deeper = root(&projects);
    deeper.depth = 2;
    assert!(find(
        &discover_root(&deeper).unwrap(),
        &projects.join("group").join("deep")
    )
    .is_some());
}

#[test]
fn worktrees_flag_and_excludes_are_respected() {
    let base = tempfile::tempdir().unwrap();
    let projects = base.path().join("projects");
    let app = init_repo(&projects.join("app"));
    init_repo(&projects.join("skip-me"));
    add_worktree(&app, "sib", &projects.join("app-sib"));
    add_worktree(&app, "ext", &base.path().join("app-ext"));

    let mut no_worktrees = root(&projects);
    no_worktrees.worktrees = false;
    let found = discover_root(&no_worktrees).unwrap();
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(found.iter().all(|f| f.worktree_of.is_none()));

    let mut excluded = root(&projects);
    excluded.exclude = vec![
        "skip-me/".into(),
        base.path().join("app-ext").to_string_lossy().into_owned(),
    ];
    let found = discover_root(&excluded).unwrap();
    assert!(find(&found, &projects.join("skip-me")).is_none());
    assert!(find(&found, &base.path().join("app-ext")).is_none());
    assert!(find(&found, &projects.join("app-sib")).is_some());
}

#[test]
fn removed_worktrees_are_pruned_but_missing_repositories_are_kept() {
    let base = tempfile::tempdir().unwrap();
    let projects = base.path().join("projects");
    let app = init_repo(&projects.join("app"));
    init_repo(&projects.join("lib"));
    let ext = base.path().join("app-ext");
    add_worktree(&app, "ext", &ext);

    let mut config = AppConfig {
        scan_roots: vec![root(&projects)],
        ..AppConfig::default()
    };
    assert!(refresh(&mut config));
    assert_eq!(config.repositories.len(), 3);
    assert!(!refresh(&mut config), "second run is a no-op");
    assert!(!worktree_gone(&ext, &projects.join("app")));

    fs::remove_dir_all(&ext).unwrap();
    fs::remove_dir_all(projects.join("lib")).unwrap();
    assert!(worktree_gone(&ext, &projects.join("app")));
    assert!(refresh(&mut config));
    let paths: Vec<String> = config.repositories.iter().map(|r| key(&r.path)).collect();
    assert!(!paths.contains(&key(&ext)), "worktree pruned: {paths:?}");
    assert!(
        paths.contains(&key(&projects.join("lib"))),
        "repository kept: {paths:?}"
    );

    // A worktree whose main repository is gone is kept (unknown state).
    assert!(!worktree_gone(&ext, &base.path().join("no-such-main")));
}

#[test]
fn excluded_entries_are_not_rediscovered() {
    let base = tempfile::tempdir().unwrap();
    let projects = base.path().join("projects");
    init_repo(&projects.join("app"));
    init_repo(&projects.join("old"));
    let mut config = AppConfig {
        scan_roots: vec![root(&projects)],
        ..AppConfig::default()
    };
    refresh(&mut config);
    let idx = config
        .repositories
        .iter()
        .position(|r| key(&r.path) == key(&projects.join("old")))
        .unwrap();
    let removed: RepoEntry = config.repositories.remove(idx);
    assert_eq!(record_exclusion(&mut config, &removed).unwrap().1, "old");
    assert!(!refresh(&mut config));
    assert_eq!(config.repositories.len(), 1);
}

#[test]
fn missing_path_and_non_repository_are_distinguished() {
    let base = tempfile::tempdir().unwrap();
    let plain = base.path().join("plain");
    fs::create_dir_all(&plain).unwrap();
    assert!(matches!(scan(&plain), Err(StatusError::NotARepository(_))));
    assert!(matches!(
        scan(&base.path().join("missing")),
        Err(StatusError::PathNotFound(_))
    ));
}

#[test]
fn worktree_status_reports_parent() {
    let base = tempfile::tempdir().unwrap();
    let app = init_repo(&base.path().join("app"));
    let wt = base.path().join("app-wt");
    add_worktree(&app, "wt", &wt);
    assert!(scan(&base.path().join("app"))
        .unwrap()
        .worktree_parent
        .is_none());
    let parent = scan(&wt).unwrap().worktree_parent.unwrap();
    assert_eq!(key(&parent), key(&base.path().join("app")));
}

fn row(path: &str, parent: Option<&str>, branches: &[&str]) -> ListRow {
    ListRow {
        target: ListTarget {
            index: 0,
            name: path.into(),
            path: PathBuf::from(path),
            is_current: false,
            distro: None,
            fetch: false,
        },
        scanned_at: chrono::Utc::now(),
        outcome: ScanOutcome::Ok(RepoStatus {
            unpushed_branches: branches
                .iter()
                .map(|b| UnpushedBranch {
                    name: (*b).into(),
                    ahead: 1,
                    has_upstream: true,
                })
                .collect(),
            worktree_parent: parent.map(PathBuf::from),
            ..RepoStatus::default()
        }),
        remote_url: None,
    }
}

#[test]
fn worktree_branches_are_counted_once() {
    let mut rows = vec![
        row("/r/app", None, &["main", "feature"]),
        row("/r/app-wt", Some("/r/app"), &["feature"]),
        row("/r/other", None, &["feature"]),
    ];
    dedupe_worktree_branches(&mut rows);
    let names = |r: &ListRow| match &r.outcome {
        ScanOutcome::Ok(s) => s
            .unpushed_branches
            .iter()
            .map(|b| b.name.clone())
            .collect::<Vec<_>>(),
        ScanOutcome::Err(_) => vec![],
    };
    assert_eq!(names(&rows[0]), ["main"]);
    assert_eq!(names(&rows[1]), ["feature"]);
    assert_eq!(names(&rows[2]), ["feature"]);
}
