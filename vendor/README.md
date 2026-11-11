# Native dependency security patches

These temporary path patches preserve the existing Rust binding APIs while using
upstream C security fixes unavailable in compatible published crates.

- `openssl-src`: Rust wrapper from crates.io 300.6.1+3.6.3, with official OpenSSL
  3.6.5 (released 2026-09-29). Official release archive SHA-256:
  `a2157c2830efdec3788939b00c9b0638306d3f0bbb76dc4832ee503bb397df98`.
  Source: https://github.com/openssl/openssl/releases/tag/openssl-3.6.5
- `libssh2-sys`: Rust wrapper from crates.io 0.3.3, with upstream libssh2 commit
  `740c2d33ad58720db1039031d2b6e13a3df9aab8` (2026-10-02). This revision includes
  the transport fixes absent from 0.3.3; it is a pinned development revision,
  not an official libssh2 release.
  Source: https://github.com/libssh2/libssh2/tree/740c2d33ad58720db1039031d2b6e13a3df9aab8

Both upstream revisions were available for at least seven days before adoption.
The OpenSSL wrapper omits its unused private `bin_dir` field; public APIs are unchanged.
The libssh2 build script disables system/vcpkg auto-detection so every build uses the reviewed source, requires the checked-in source rather than running git submodule update, and follows the upstream split of crypto.c into backend source files.
The Rust package versions carry local metadata to identify the patched sources.
OpenSSL tests, fuzz corpus and optional fuzz sources and demos, libssh2 test fixtures and test `.pem` / `.key` files are omitted; `removed-test-keys.json` records omissions.
The required `fuzz/build.info` and documentation sources remain for Configure; fuzz targets are disabled.
Upstream licenses and copyright notices are retained. No new direct dependency is added.

Remove each patch once the corresponding compatible official crate contains the
required fixes and satisfies the seven-day waiting period. Audit upstream native
advisories, compare the actual bundled sources, update the lockfile and provenance
checks, then validate Linux musl and Windows again. Dependabot does not update these
vendored sources automatically; maintainers must monitor upstream advisories.
