use clap::Parser;
use git_clawler_wsl_helper::{
    run_discover, run_scan, run_scan_root, run_worktree_gone, DiscoverResponse, ScanResponse,
    ScanRootResponse, WorktreeGoneResponse, HELPER_VERSION, PROTOCOL_VERSION,
};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser, Debug)]
#[command(name = "git-clawler-wsl-helper", version)]
struct Cli {
    /// Repository paths (absolute) to scan
    #[arg(value_name = "PATH", num_args = 0..)]
    paths: Vec<PathBuf>,

    /// Print protocol version handshake JSON and exit
    #[arg(long)]
    protocol: bool,

    /// Run `git fetch` on each repository's current-branch upstream before
    /// scanning. Failures are logged and the scan proceeds with the existing
    /// remote-tracking cache.
    #[arg(long, default_value_t = false)]
    fetch: bool,

    /// Discover repositories below this absolute scan root instead of scanning
    /// the positional repository paths.
    #[arg(long, value_name = "ROOT")]
    scan_root: Option<PathBuf>,

    /// Maximum directory depth for --scan-root.
    #[arg(long, default_value_t = 3)]
    depth: u32,

    /// Exclusion for --scan-root/--discover: a directory name, a path relative
    /// to the root, or an absolute path. May be repeated.
    #[arg(long = "exclude", value_name = "NAME")]
    exclude: Vec<String>,

    /// Discover repositories and their linked worktrees below this root.
    #[arg(long, value_name = "ROOT")]
    discover: Option<PathBuf>,

    /// With --discover: do not report linked worktrees.
    #[arg(long, default_value_t = false)]
    no_worktrees: bool,

    /// Report which registered worktrees Git no longer tracks. Pair each
    /// --worktree with a --parent (main repository) in the same order.
    #[arg(long, default_value_t = false)]
    worktree_gone: bool,

    #[arg(long = "worktree", value_name = "PATH")]
    worktrees: Vec<PathBuf>,

    #[arg(long = "parent", value_name = "PATH")]
    parents: Vec<PathBuf>,
}

fn main() -> ExitCode {
    // SAFETY: configure global libgit2 settings before creating the runtime.
    if let Err(error) = unsafe { git_clawler_core::status::configure_network_timeouts() } {
        eprintln!("failed to configure Git socket deadlines: {error}");
        return ExitCode::FAILURE;
    }
    git_clawler_core::fetch_worker::run_if_requested();
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .try_init();

    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            let code = e.exit_code();
            let _ = e.print();
            return ExitCode::from(code as u8);
        }
    };

    if cli.protocol {
        let handshake = git_clawler_wsl_helper::protocol::HelperHandshake {
            protocol_version: PROTOCOL_VERSION,
            helper_version: HELPER_VERSION.to_string(),
            capabilities: git_clawler_wsl_helper::protocol::HelperCapabilities {
                populates_remote_url: true,
                scans_roots: true,
                populates_recent_commits: true,
                discovers_roots: true,
                reports_worktrees: true,
            },
        };
        println!("{}", serde_json::to_string(&handshake).unwrap());
        return ExitCode::SUCCESS;
    }

    // wsl.exe termination does not guarantee Linux-process termination.
    // Keep an in-process watchdog so a disconnected launcher cannot leave a
    // stalled helper running indefinitely. Exiting terminates blocking workers.
    let budget = std::time::Duration::from_secs(if cli.fetch { 120 } else { 60 });
    let (completion_tx, completion_rx) = std::sync::mpsc::channel::<()>();
    let _completion_guard = completion_tx;
    if let Err(error) = std::thread::Builder::new()
        .name("helper-deadline".into())
        .spawn(move || {
            if matches!(
                completion_rx.recv_timeout(budget),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ) {
                eprintln!(
                    "WSL helper timed out after {} seconds; retry the operation",
                    budget.as_secs()
                );
                std::process::exit(1);
            }
        })
    {
        eprintln!("failed to start helper deadline: {error}");
        return ExitCode::FAILURE;
    }

    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!(error = %e, "failed to build tokio runtime");
            return ExitCode::from(1);
        }
    };

    let modes = usize::from(cli.scan_root.is_some())
        + usize::from(cli.discover.is_some())
        + usize::from(cli.worktree_gone);
    if modes > 1 || (modes == 1 && !cli.paths.is_empty()) {
        eprintln!(
            "--scan-root, --discover and --worktree-gone are mutually exclusive and cannot be combined with repository paths"
        );
        return ExitCode::from(2);
    }
    if cli.worktrees.len() != cli.parents.len() {
        eprintln!("each --worktree needs a matching --parent");
        return ExitCode::from(2);
    }

    let response = if let Some(root) = cli.discover {
        HelperOutput::Discover(rt.block_on(run_discover(
            root,
            cli.depth,
            cli.exclude,
            !cli.no_worktrees,
        )))
    } else if cli.worktree_gone {
        let pairs = cli.worktrees.into_iter().zip(cli.parents).collect();
        HelperOutput::Gone(rt.block_on(run_worktree_gone(pairs)))
    } else if let Some(root) = cli.scan_root {
        HelperOutput::Root(rt.block_on(run_scan_root(root, cli.depth, cli.exclude)))
    } else {
        HelperOutput::Repos(rt.block_on(run_scan(cli.paths, cli.fetch)))
    };
    match response.to_json() {
        Ok(s) => {
            println!("{s}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to serialize response");
            ExitCode::from(1)
        }
    }
}

enum HelperOutput {
    Repos(ScanResponse),
    Root(ScanRootResponse),
    Discover(DiscoverResponse),
    Gone(WorktreeGoneResponse),
}

impl HelperOutput {
    fn to_json(&self) -> serde_json::Result<String> {
        match self {
            Self::Repos(response) => serde_json::to_string(response),
            Self::Root(response) => serde_json::to_string(response),
            Self::Discover(response) => serde_json::to_string(response),
            Self::Gone(response) => serde_json::to_string(response),
        }
    }
}
