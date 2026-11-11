use clap::Parser;
use git_clawler_wsl_helper::{
    run_scan, run_scan_root, ScanResponse, ScanRootResponse, HELPER_VERSION, PROTOCOL_VERSION,
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

    /// Basename to exclude during --scan-root traversal. May be repeated.
    #[arg(long = "exclude", value_name = "NAME")]
    exclude: Vec<String>,
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

    if cli.scan_root.is_some() && !cli.paths.is_empty() {
        eprintln!("--scan-root cannot be combined with repository path arguments");
        return ExitCode::from(2);
    }

    let response = if let Some(root) = cli.scan_root {
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
}

impl HelperOutput {
    fn to_json(&self) -> serde_json::Result<String> {
        match self {
            Self::Repos(response) => serde_json::to_string(response),
            Self::Root(response) => serde_json::to_string(response),
        }
    }
}
