"""Block distribution of dependency versions with known unresolved advisories.

RustSec does not cover all vendored C libraries. Changes to these constraints
require a fresh review of upstream advisories and the actual bundled source.
"""
from pathlib import Path
import tomllib
import json
import hashlib

root = Path(__file__).resolve().parents[1]
packages = tomllib.loads((root / "Cargo.lock").read_text())["package"]
versions = {p["name"]: p["version"] for p in packages}
errors = []
def numeric(value):
    return tuple(int(part) for part in value.split("+")[0].split(".")[:3])

# GitHub-reviewed advisories not yet covered by the RustSec database.
# https://github.com/tauri-apps/tauri/security/advisories/GHSA-7gmj-67g7-phm9
# https://github.com/jonasbb/serde_with/security/advisories/GHSA-7gcf-g7xr-8hxj
for name, minimum, advisory in (
    ("tauri", (2, 11, 1), "GHSA-7gmj-67g7-phm9"),
    ("serde_with", (3, 21, 0), "GHSA-7gcf-g7xr-8hxj"),
):
    matches = [p["version"] for p in packages if p["name"] == name]
    if not matches or any("-" in version or numeric(version) < minimum for version in matches):
        errors.append(f"{name} must include {advisory} fixes ({'.'.join(map(str, minimum))})")

libgit2 = versions.get("libgit2-sys", "").split("+")[-1]
if not libgit2 or numeric(libgit2) < (1, 9, 7):
    errors.append("libgit2 must include the 1.9.7 security fixes")
openssl = versions.get("openssl-src", "").split("+")[-1]
if not openssl or not (
    ((3, 6, 5) <= numeric(openssl) < (3, 7, 0))
    or ((4, 0, 3) <= numeric(openssl) < (4, 1, 0))
):
    errors.append("OpenSSL must include the September 2026 fixes (3.6.5 / 4.0.3)")
# Registry libssh2-sys 0.3.3 ships an older transport.c without the ETM length check
# from upstream 42e33d8. Do not assume an unchanged lock version is patched.
ssh = versions.get("libssh2-sys")
patched_ssh = ssh == "0.3.3+security.20261012" and not next(p for p in packages if p["name"] == "libssh2-sys").get("source")
if (root / "vendor/PROVENANCE.json").is_file():
    provenance = json.loads((root / "vendor/PROVENANCE.json").read_text())
    if provenance["libssh2"]["native_commit"] != "740c2d33ad58720db1039031d2b6e13a3df9aab8":
        errors.append("libssh2 source revision has not been reviewed")
    expected = {item["path"] for item in provenance["files"]}
    actual = {str(path.relative_to(root / "vendor")) for path in (root / "vendor").rglob("*") if path.is_file() and path != root / "vendor/PROVENANCE.json"}
    if actual != expected:
        errors.append("vendored file inventory changed")
    for item in provenance["files"]:
        path = root / "vendor" / item["path"]
        if not path.is_file() or hashlib.sha256(path.read_bytes()).hexdigest() != item["sha256"]:
            errors.append("vendored source changed: " + item["path"])
            break
else:
    errors.append("vendored provenance is missing")
if not patched_ssh:
    errors.append("libssh2 bundled transport fixes require source review (CVE-2026-66035)")
if errors:
    raise SystemExit("Release blocked:\n- " + "\n- ".join(errors))
print("dependency release constraints passed")
