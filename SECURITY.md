# Security policy

The supported release series is 1.x. Please report vulnerabilities privately through
[GitHub private vulnerability reporting](https://github.com/sahokun/git-clawler/security/advisories/new).
Include affected versions, operating system, reproduction steps and impact. Do not open
public issues containing exploit details or credentials. If private reporting is unavailable,
wait for the maintainer to enable it; do not publish sensitive details in an issue.

Fetch and GitHub counts are opt-in network operations. Tokens belong in environment variables
or the OS credential store. Windows 1.0.0 binaries are unsigned. Review SHA256SUMS before use.

Reviewed Tauri maintenance warnings and unreachable advisory paths are documented in
`deny.toml`, with a review deadline of 2027-01-10 and a tracking issue. Applicable
native security fixes use the temporary, provenance-checked sources described in
[vendor/README.md](vendor/README.md). Maintainers monitor these sources manually;
Dependabot updates registry packages but does not replace vendored C code.
