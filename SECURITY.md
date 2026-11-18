# Security policy

The supported release series is 1.x. Please report vulnerabilities privately through
[GitHub private vulnerability reporting](https://github.com/sahokun/git-clawler/security/advisories/new).
Include affected versions, operating system, reproduction steps and impact. Do not open
public issues containing exploit details or credentials. If private reporting is unavailable,
wait for the maintainer to enable it; do not publish sensitive details in an issue.

Fetch and GitHub counts are opt-in network operations. Tokens belong in environment variables
or the OS credential store. Windows binaries are unsigned. Review SHA256SUMS before use.

Reviewed Rust advisory exceptions (unmaintained crates and paths unreachable on supported
targets) are documented in `deny.toml`; npm exceptions, limited to advisories reachable only
through build and test tooling, are in `npm-audit.toml`. Each exception has a tracking issue
and a review deadline that the audit scripts enforce. Applicable
native security fixes use the temporary, provenance-checked sources described in
[vendor/README.md](vendor/README.md). Maintainers monitor these sources manually;
Dependabot updates registry packages but does not replace vendored C code.
