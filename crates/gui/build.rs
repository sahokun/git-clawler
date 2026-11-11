fn main() {
    // NOTE: `#[cfg(target_os = "windows")]` はコンパイルターゲットではなく
    // **ビルドを実行するホスト OS** で評価される (Cargo ビルドスクリプトの仕様)。
    // したがって Linux ホストから Windows 向けクロスコンパイルを行う場合、
    // この条件は false になり `tauri_build::build()` が呼び出されない。
    // クロスコンパイルが必要になった際は CARGO_CFG_TARGET_OS 環境変数を使うこと:
    //   if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") { ... }
    // 現状: Windows ホストでのネイティブビルドのみサポート。クロスコンパイルは未サポート。
    #[cfg(target_os = "windows")]
    tauri_build::build()
}
