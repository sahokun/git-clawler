use std::path::Path;

use crate::render::render_list_table;
use crate::scan::{build_targets, scan_all, scan_all_with_github};

pub async fn run(with_github: bool) -> anyhow::Result<()> {
    let config_path = git_clawler_core::config::default_config_path()?;
    let config_exists = config_path.try_exists().unwrap_or(false);
    let config = git_clawler_core::config::load_or_default()?;

    let targets = build_targets(&config);
    if targets.is_empty() {
        print_empty_message(config_exists, &config_path);
        return Ok(());
    }

    let show_github = config.github.enabled || with_github;
    let rows = if show_github {
        let mut github_cfg = config.github.clone();
        github_cfg.enabled = true;
        scan_all_with_github(targets, &github_cfg).await
    } else {
        scan_all(targets).await
    };

    use std::io::IsTerminal;
    let color = std::io::stdout().is_terminal();
    println!("{}", render_list_table(&rows, color, show_github));
    print_footer();
    Ok(())
}

fn print_footer() {
    println!();
    println!(
        "  * = current directory   Uncommitted = uncommitted files   Push = unpushed branch count"
    );
    println!("  A/B = ahead/behind");
}

fn print_empty_message(config_exists: bool, path: &Path) {
    if !config_exists {
        eprintln!(
            "No repositories are configured ({} does not exist yet).",
            path.display()
        );
    } else {
        eprintln!("No repositories are configured ({}).", path.display());
    }
    eprintln!("Add repositories by editing the config file or using `gclawl scan <root> --add`.");
}
