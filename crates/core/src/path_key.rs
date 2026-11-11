//! Path identity keys shared by config, registry, and frontends.
//!
//! Duplicate detection intentionally avoids filesystem canonicalization so it
//! also works for missing paths, WSL paths, and paths without read permission.
//! `..` components are preserved because collapsing them can cross symlink or
//! junction boundaries.

use std::borrow::Cow;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativePathPlatform {
    Posix,
    Windows,
}

impl NativePathPlatform {
    pub fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Posix
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DistroPathKey {
    distro: Option<String>,
    path: String,
}

impl DistroPathKey {
    pub fn current_host(distro: Option<&str>, path: &Path) -> Self {
        Self::for_native_platform(distro, path, NativePathPlatform::current())
    }

    pub fn for_native_platform(
        distro: Option<&str>,
        path: &Path,
        native_platform: NativePathPlatform,
    ) -> Self {
        let distro = normalize_distro_ref(distro);
        let is_default_wsl_path = native_platform == NativePathPlatform::Windows
            && distro.is_none()
            && path_starts_with_posix_root(path);
        let platform = if distro.is_some() || is_default_wsl_path {
            NativePathPlatform::Posix
        } else {
            native_platform
        };
        Self {
            distro,
            path: normalize_path_for_platform(path, platform),
        }
    }

    pub fn distro(&self) -> Option<&str> {
        self.distro.as_deref()
    }

    pub fn path_key(&self) -> &str {
        &self.path
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PathIdentityKey(DistroPathKey);

impl PathIdentityKey {
    pub fn current_host(distro: Option<&str>, path: &Path) -> Self {
        Self(DistroPathKey::current_host(distro, path))
    }

    pub fn for_native_platform(
        distro: Option<&str>,
        path: &Path,
        native_platform: NativePathPlatform,
    ) -> Self {
        Self(DistroPathKey::for_native_platform(
            distro,
            path,
            native_platform,
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PathMatchKey(PathBuf);

impl PathMatchKey {
    pub fn new(path: &Path) -> Self {
        Self(path_match_key(path))
    }

    pub fn into_path_buf(self) -> PathBuf {
        self.0
    }
}

impl AsRef<Path> for PathMatchKey {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

pub fn normalize_distro_ref(distro: Option<&str>) -> Option<String> {
    distro.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn normalize_path_for_platform(path: &Path, platform: NativePathPlatform) -> String {
    match platform {
        NativePathPlatform::Posix => normalize_posix_path_for_path(path),
        NativePathPlatform::Windows => normalize_windows_path(&path.to_string_lossy()),
    }
}

fn path_starts_with_posix_root(path: &Path) -> bool {
    #[cfg(unix)]
    {
        path.as_os_str().as_bytes().starts_with(b"/")
    }
    #[cfg(not(unix))]
    {
        path.to_string_lossy().starts_with('/')
    }
}

fn normalize_posix_path_for_path(path: &Path) -> String {
    #[cfg(unix)]
    {
        normalize_posix_path_bytes(path.as_os_str().as_bytes())
    }
    #[cfg(not(unix))]
    {
        normalize_posix_path(&path.to_string_lossy())
    }
}

#[cfg(not(unix))]
fn normalize_posix_path(raw: &str) -> String {
    normalize_posix_path_bytes(raw.as_bytes())
}

fn normalize_posix_path_bytes(raw: &[u8]) -> String {
    let absolute = raw.starts_with(b"/");
    let mut byte_parts: Vec<&[u8]> = Vec::new();
    for part in raw.split(|byte| *byte == b'/') {
        match part {
            b"" | b"." => {}
            b".." => byte_parts.push(b".."),
            other => byte_parts.push(other),
        }
    }

    let mut normalized = Vec::new();
    if absolute {
        normalized.push(b'/');
    }
    for (index, part) in byte_parts.iter().enumerate() {
        if index > 0 {
            normalized.push(b'/');
        }
        normalized.extend_from_slice(part);
    }
    if normalized.is_empty() {
        ".".to_string()
    } else if let Ok(valid) = String::from_utf8(normalized.clone()) {
        valid
    } else {
        encode_non_utf8_posix_key(&normalized)
    }
}

fn encode_non_utf8_posix_key(bytes: &[u8]) -> String {
    let mut encoded = String::from("\0posix-bytes:");
    for byte in bytes {
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

fn normalize_windows_path(raw: &str) -> String {
    let stripped = strip_windows_long_path_prefix(raw);
    let lower = stripped.replace('/', "\\").to_lowercase();
    let prefix = windows_prefix_len(&lower);
    let (prefix_text, rest) = lower.split_at(prefix);
    let mut parts: Vec<&str> = Vec::new();
    for part in rest.split('\\') {
        match part {
            "" | "." => {}
            ".." => parts.push(".."),
            other => parts.push(other),
        }
    }

    let mut normalized = prefix_text.to_string();
    if is_unc_share_prefix(&normalized) && !normalized.ends_with('\\') {
        normalized.push('\\');
    }
    if !normalized.is_empty() && !normalized.ends_with('\\') && !parts.is_empty() {
        normalized.push('\\');
    }
    normalized.push_str(&parts.join("\\"));
    if normalized.is_empty() {
        ".".to_string()
    } else {
        normalized
    }
}

fn strip_windows_long_path_prefix(raw: &str) -> Cow<'_, str> {
    const UNC_PREFIX: &str = r"\\?\UNC\";
    const LONG_PREFIX: &str = r"\\?\";

    if raw
        .get(..UNC_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(UNC_PREFIX))
    {
        return Cow::Owned(format!(r"\\{}", &raw[UNC_PREFIX.len()..]));
    }
    if raw
        .get(..LONG_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(LONG_PREFIX))
    {
        return Cow::Borrowed(&raw[LONG_PREFIX.len()..]);
    }
    Cow::Borrowed(raw)
}

fn is_unc_share_prefix(prefix: &str) -> bool {
    if !prefix.starts_with(r"\\") {
        return false;
    }
    let mut components = prefix.trim_start_matches('\\').split('\\');
    let Some(server) = components.next() else {
        return false;
    };
    let Some(share) = components.next() else {
        return false;
    };
    !server.is_empty() && !share.is_empty()
}

fn windows_prefix_len(path: &str) -> usize {
    let bytes = path.as_bytes();
    if bytes.len() >= 3 && bytes[1] == b':' && bytes[2] == b'\\' {
        return 3;
    }
    if path.starts_with(r"\\") {
        let mut separators = 0;
        for (idx, byte) in bytes.iter().enumerate().skip(2) {
            if *byte == b'\\' {
                separators += 1;
                if separators == 2 {
                    return idx + 1;
                }
            }
        }
        return path.len();
    }
    if path.starts_with('\\') {
        return 1;
    }
    0
}

fn path_match_key(path: &Path) -> PathBuf {
    match std::fs::canonicalize(path) {
        Ok(canonical) => canonical,
        Err(_) => lexical_absolute(path),
    }
}

fn lexical_absolute(path: &Path) -> PathBuf {
    let base = if path.is_absolute() {
        PathBuf::new()
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    };
    let mut out = base;
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use std::ffi::OsString;
    #[cfg(unix)]
    use std::os::unix::ffi::OsStringExt;

    #[test]
    fn distro_is_trimmed_and_blank_becomes_none() {
        assert_eq!(
            normalize_distro_ref(Some(" Ubuntu ")),
            Some("Ubuntu".into())
        );
        assert_eq!(normalize_distro_ref(Some("  ")), None);
        assert_eq!(normalize_distro_ref(None), None);
    }

    #[test]
    fn posix_native_key_is_case_sensitive() {
        let a = DistroPathKey::for_native_platform(
            None,
            Path::new("/home/user/Repo"),
            NativePathPlatform::Posix,
        );
        let b = DistroPathKey::for_native_platform(
            None,
            Path::new("/home/user/repo"),
            NativePathPlatform::Posix,
        );
        assert_ne!(a, b);
    }

    #[cfg(unix)]
    #[test]
    fn posix_non_utf8_paths_keep_distinct_byte_keys() {
        let invalid_a = OsString::from_vec(vec![b'/', b't', b'm', b'p', b'/', 0xff, b'a']);
        let invalid_b = OsString::from_vec(vec![b'/', b't', b'm', b'p', b'/', 0xff, b'b']);
        let a = DistroPathKey::for_native_platform(
            None,
            Path::new(&invalid_a),
            NativePathPlatform::Posix,
        );
        let b = DistroPathKey::for_native_platform(
            None,
            Path::new(&invalid_b),
            NativePathPlatform::Posix,
        );

        assert_ne!(a, b);
        assert_eq!(a.path_key(), "\0posix-bytes:2f746d702fff61");
        assert_eq!(b.path_key(), "\0posix-bytes:2f746d702fff62");
    }

    #[cfg(unix)]
    #[test]
    fn posix_non_utf8_paths_do_not_collide_with_lossy_utf8_replacement() {
        let invalid = OsString::from_vec(vec![b'/', b't', b'm', b'p', b'/', 0xff]);
        let replacement = OsString::from("/tmp/�");
        let invalid_key = DistroPathKey::for_native_platform(
            None,
            Path::new(&invalid),
            NativePathPlatform::Posix,
        );
        let replacement_key = DistroPathKey::for_native_platform(
            None,
            Path::new(&replacement),
            NativePathPlatform::Posix,
        );

        assert_ne!(invalid_key, replacement_key);
    }

    #[test]
    fn wsl_distro_key_is_posix_and_case_sensitive() {
        let a = DistroPathKey::for_native_platform(
            Some("Ubuntu"),
            Path::new("/home/user/Repo"),
            NativePathPlatform::Windows,
        );
        let b = DistroPathKey::for_native_platform(
            Some("Ubuntu"),
            Path::new("/home/user/repo"),
            NativePathPlatform::Windows,
        );
        assert_ne!(a, b);
    }

    #[test]
    fn windows_native_key_strips_long_prefix_and_ignores_case() {
        let a = DistroPathKey::for_native_platform(
            None,
            Path::new(r"\\?\C:\Users\Me\Repo"),
            NativePathPlatform::Windows,
        );
        let b = DistroPathKey::for_native_platform(
            None,
            Path::new(r"c:\users\me\repo"),
            NativePathPlatform::Windows,
        );
        assert_eq!(a, b);
    }

    #[test]
    fn windows_native_key_preserves_unc_long_prefix_semantics() {
        let a = DistroPathKey::for_native_platform(
            None,
            Path::new(r"\\?\UNC\Server\Share\Repo"),
            NativePathPlatform::Windows,
        );
        let b = DistroPathKey::for_native_platform(
            None,
            Path::new(r"\\server\share\repo"),
            NativePathPlatform::Windows,
        );
        assert_eq!(a, b);
    }

    #[test]
    fn windows_native_key_strips_unc_long_prefix_case_insensitively() {
        let a = DistroPathKey::for_native_platform(
            None,
            Path::new(r"\\?\unc\Server\Share\Repo"),
            NativePathPlatform::Windows,
        );
        let b = DistroPathKey::for_native_platform(
            None,
            Path::new(r"\\server\share\repo"),
            NativePathPlatform::Windows,
        );
        assert_eq!(a, b);
    }

    #[test]
    fn malformed_long_prefix_with_non_ascii_does_not_panic() {
        let key = DistroPathKey::for_native_platform(
            None,
            Path::new(r"\\?\UNCé\Server\Share"),
            NativePathPlatform::Windows,
        );
        assert_eq!(key.path_key(), r"uncé\server\share");
    }

    #[test]
    fn windows_native_unc_share_root_trailing_separator_is_ignored() {
        let a = DistroPathKey::for_native_platform(
            None,
            Path::new(r"\\server\share"),
            NativePathPlatform::Windows,
        );
        let b = DistroPathKey::for_native_platform(
            None,
            Path::new(r"\\server\share\"),
            NativePathPlatform::Windows,
        );
        assert_eq!(a, b);
        assert_eq!(a.path_key(), r"\\server\share\");
    }

    #[test]
    fn lexical_normalization_avoids_filesystem_lookup_but_preserves_parent_dirs() {
        let key = DistroPathKey::for_native_platform(
            None,
            Path::new("/definitely/missing/../missing/repo"),
            NativePathPlatform::Posix,
        );
        assert_eq!(key.path_key(), "/definitely/missing/../missing/repo");
    }

    #[test]
    fn parent_dirs_do_not_collapse_across_possible_symlinks() {
        let a = DistroPathKey::for_native_platform(
            None,
            Path::new("/a/link/../repo"),
            NativePathPlatform::Posix,
        );
        let b = DistroPathKey::for_native_platform(
            None,
            Path::new("/a/repo"),
            NativePathPlatform::Posix,
        );
        assert_ne!(a, b);
    }

    #[test]
    fn default_wsl_paths_are_posix_on_windows_host() {
        let a = DistroPathKey::for_native_platform(
            None,
            Path::new("/home/me/Repo"),
            NativePathPlatform::Windows,
        );
        let b = DistroPathKey::for_native_platform(
            None,
            Path::new("/home/me/repo"),
            NativePathPlatform::Windows,
        );
        assert_ne!(a, b);
    }
}
