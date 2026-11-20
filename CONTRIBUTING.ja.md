# コントリビュート

正本は [CONTRIBUTING.md](CONTRIBUTING.md) です。fork から main へ PR を作成し、CI 成功後にメンテナがレビュー・squash merge します。
Rust は 1.93.1、Node は `crates/gui/.nvmrc` に合わせます。Linux は cmake / pkg-config / libssl-dev / build-essential、
Windows は MSVC C++ ビルドツール・Windows SDK・WebView2 が必要です。

`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、
`cargo test --workspace --locked`、`cargo deny check` を実行してください。
GUI は `npm ci` 後に `npm test`、`npm run build` を実行し、リポジトリ直下で `python3 scripts/audit-npm.py`（`npm-audit.toml` の審査済み例外つきの npm audit）を実行します。
Windows 専用変更は Windows でも検証します。helper の配置・GUI 配布ビルド手順は英語版を参照してください。

Conventional Commits で変更目的を分け、挙動変更には回帰テストを付けます。
新規依存は公式レジストリで実在・保守・ライセンス・advisory を確認し、推移的依存を含め公開7日以上の版のみ採用します。
例外は理由・対象・Issue・期限を明記し、期限を黙って延長しません。Git・設定処理は core に置きます。
依存更新では `python3 scripts/refresh-deps.py check`（公開7日・yanked/deprecated・Tauri の Rust/JS 版ずれを検査、`--markdown FILE` で PR 用の表を出力）を実行します。`fix` は新しすぎる crate の固定とライセンス補足の追加まで行います。
`THIRD_PARTY_NOTICES.txt` はコミットせず、リリースのワークフローが `python3 scripts/refresh-deps.py notices` で lockfile から生成して配布物に同梱します。PR の dependency policy チェックも同じコマンドを実行し、配布する依存にライセンス文が無ければ失敗します。その場合は `fix`（または `licenses/` への手作業の追加）で補足を足します。ローカルでポータブル版を作るときは、`crates/gui` で `npm ci` を実行してから生成します。
公開文書は英語を正本とし、大きな変更は日本語版にも反映します。機密情報や個人設定を含めないでください。

## Node.jsの修正基準

開発・CIは`crates/gui/.nvmrc`のNode 26.13.0を使用します。enginesはCVE-2026-111497修正済みの22/24/26系列を許容しますが、今回の検証版は26.13.0です。nvmでは`crates/gui`で`nvm install && nvm use`、Windowsでは`nvm install 26.13.0`と`nvm use 26.13.0`を実行します。新しい端末で`node --version`と`node -p "process.execPath"`を確認し、開発プロセスを再起動してください。Actions用ランタイムは別なのでランナー自体も更新します。
