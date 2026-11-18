"""Audit the lockfile using the reviewed exceptions shared with cargo-deny."""
from pathlib import Path
from datetime import date
import re
import subprocess
import tomllib

root = Path(__file__).resolve().parents[1]
text = (root / "deny.toml").read_text()
# Every exception group carries its own deadline; the earliest one governs.
expiries = [date.fromisoformat(d) for d in re.findall(r"Review expires (\d{4}-\d{2}-\d{2})", text)]
if not expiries or date.today() > min(expiries):
    raise SystemExit("Advisory exceptions require a new review before auditing")
config = tomllib.loads(text)
command = ["cargo", "audit", "--deny", "warnings"]
for advisory in config["advisories"]["ignore"]:
    command.extend(["--ignore", advisory])
raise SystemExit(subprocess.call(command, cwd=root))
