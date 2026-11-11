"""Reject releases whose tag disagrees with any application manifest."""
import json
from pathlib import Path
import sys
import tomllib

root = Path(__file__).resolve().parents[1]
version = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]["package"]["version"]
if len(sys.argv) > 1 and sys.argv[1] != "v" + version:
    raise SystemExit("tag must match workspace version")
for name in ("package.json", "package-lock.json", "tauri.conf.json"):
    data = json.loads((root / "crates/gui" / name).read_text())
    if data["version"] != version:
        raise SystemExit(f"version mismatch: {name}")
    if name == "package-lock.json" and data["packages"][""]["version"] != version:
        raise SystemExit("package-lock root version mismatch")
lock = tomllib.loads((root / "Cargo.lock").read_text())
for package in lock["package"]:
    if package["name"] in ("git-clawler", "git-clawler-core", "git-clawler-gui", "git-clawler-wsl-helper"):
        if package["version"] != version:
            raise SystemExit("Cargo.lock version mismatch")
# The Tauri bundler requires matching Rust / JavaScript API major and minor.
rust_tauri = [p["version"] for p in lock["package"] if p["name"] == "tauri"]
js_lock = json.loads((root / "crates/gui/package-lock.json").read_text())
js_tauri = js_lock["packages"]["node_modules/@tauri-apps/api"]["version"]
if len(rust_tauri) != 1 or rust_tauri[0].split(".")[:2] != js_tauri.split(".")[:2]:
    raise SystemExit("Tauri Rust and JavaScript API major/minor versions must match")
print(f"release versions agree: {version}")
