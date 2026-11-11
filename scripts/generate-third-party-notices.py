"""Collect dependency license declarations and available license texts."""
import json
from pathlib import Path
import subprocess
import sys

root = Path(__file__).resolve().parents[1]
metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--format-version", "1"], cwd=root))
sections = ["Third-party notices for git-clawler\nGenerated from Cargo.lock and npm package-lock.json.\nIncludes build and platform dependencies; not all are shipped on each target.\n"]
missing = []
for package in sorted(metadata["packages"], key=lambda p: (p["name"], p["version"])):
    if package["id"] in metadata["workspace_members"]:
        continue
    base = Path(package["manifest_path"]).parent
    texts = []
    for path in sorted(base.rglob("*")):
        if path.is_file() and path.name.lower().startswith(("license", "licence", "copying", "copyright", "notice")):
            texts.append(path.read_text(errors="replace"))
    if package.get("license_file"):
        path = base / package["license_file"]
        if path.is_file():
            texts.append(path.read_text(errors="replace"))
    supplement = root / "licenses" / (package["name"] + "-" + package["version"] + ".txt")
    if supplement.is_file():
        texts.append(supplement.read_text())
    sections.append(f"\n===== Rust: {package['name']} {package['version']} =====\nLicense: {package.get('license')}\nRepository: {package.get('repository')}\nAuthors: {package.get('authors')}\n" + "\n".join(texts))
    if not texts:
        missing.append("Rust: " + package["name"])
lock = json.loads((root / "crates/gui/package-lock.json").read_text())
for name, entry in sorted(lock["packages"].items()):
    if not name:
        continue
    base = root / "crates/gui" / name
    texts = []
    if base.is_dir():
        for path in sorted(base.iterdir()):
            if path.is_file() and path.name.lower().startswith(("license", "licence", "copying", "notice")):
                texts.append(path.read_text(errors="replace"))
    supplement = root / "licenses" / ("npm-" + name.removeprefix("node_modules/").replace("/", "_") + "-" + entry.get("version", "") + ".txt")
    if supplement.is_file():
        texts.append(supplement.read_text())
    if not texts and name.startswith("node_modules/@radix-ui/"):
        umbrella = root / "crates/gui/node_modules/@radix-ui/react-dialog/LICENSE"
        if umbrella.is_file():
            texts.append("Radix Primitives MIT license (WorkOS):\n" + umbrella.read_text())
    sections.append(f"\n===== npm: {name} {entry.get('version')} =====\nLicense: {entry.get('license')}\n" + "\n".join(texts))
    if not texts:
        missing.append("npm: " + name)
(root / "THIRD_PARTY_NOTICES.txt").write_text("\n".join(sections))
print("License texts requiring manual verification:", *missing, sep="\n", file=sys.stderr)
