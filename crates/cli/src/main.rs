use std::path::PathBuf;

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "gclawl", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// List all registered repositories with their status
    List {
        /// Fetch GitHub issue and pull request counts for GitHub remotes
        #[arg(long)]
        with_github: bool,
    },
    /// Recursively discover git repositories under <root>
    Scan {
        /// Root directory to scan (absolute or relative)
        root: PathBuf,
        /// Register newly discovered repositories into the config file
        #[arg(long)]
        add: bool,
        /// Maximum recursion depth (default: 3)
        #[arg(long)]
        depth: Option<u32>,
    },
    /// Remove a repository entry from the config without deleting files
    Remove {
        /// Repository path to remove from the config
        path: PathBuf,
    },
    /// Watch repositories and refresh on filesystem changes (TUI)
    Watch {
        /// Fetch GitHub issue and pull request counts for GitHub remotes
        #[arg(long)]
        with_github: bool,
    },
    /// Open the interactive terminal UI
    Tui {
        /// Fetch GitHub issue and pull request counts for GitHub remotes
        #[arg(long)]
        with_github: bool,
    },
}

fn main() -> anyhow::Result<()> {
    // SAFETY: no runtime or worker threads have been started yet.
    unsafe { git_clawler_core::status::configure_network_timeouts()? };
    git_clawler_core::fetch_worker::run_if_requested();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run())
}

async fn run() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .init();

    let cli = Cli::parse();
    match cli.command {
        Commands::List { with_github } => git_clawler_cli::commands::list::run(with_github).await,
        Commands::Scan { root, add, depth } => {
            git_clawler_cli::commands::scan::run(root, add, depth).await
        }
        Commands::Remove { path } => git_clawler_cli::commands::remove::run(path).await,
        Commands::Watch { with_github } => git_clawler_cli::commands::watch::run(with_github).await,
        Commands::Tui { with_github } => {
            let config = git_clawler_core::config::load_or_default()?;
            tokio::task::spawn_blocking(move || git_clawler_cli::tui::run(config, with_github))
                .await?
        }
    }
}
