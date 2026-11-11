#[tokio::test]
async fn watch_run_fails_without_tty() {
    if git_clawler_cli::term::TerminalGuard::is_tty() {
        return;
    }
    let result = git_clawler_cli::commands::watch::run(false).await;
    assert!(
        result.is_err(),
        "watch should fail when stdout is not a TTY"
    );
    let err = result.unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("tty"),
        "error was: {}",
        err
    );
}
