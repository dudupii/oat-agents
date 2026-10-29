// SPDX-License-Identifier: MIT
// Copyright (c) 2026 oat-agents contributors

//! The content hash an in-repository plugin pins to, and the embedded example checks itself
//! against (ticket Architecture): SHA-256 over the sorted list of (relative path, file mode,
//! file bytes) of every file under a directory, so a rename, a mode change or an edit all
//! change it.

use sha2::{Digest, Sha256};
use std::fs;
use std::io;
use std::path::Path;

/// One file's identity for hashing: a POSIX-style relative path, its Unix permission bits (or
/// `0o644` where the platform has none), and its bytes.
pub struct HashedFile<'a> {
    pub relative_path: String,
    pub mode: u32,
    pub bytes: &'a [u8],
}

/// The canonical hash over an already-sorted, already-deduplicated list of files. Both the
/// on-disk hasher (`hash_dir`) and the embedded example's own self-check hash through this, so
/// the two can never drift by using two different encodings.
pub fn hash_entries<'a>(entries: impl IntoIterator<Item = HashedFile<'a>>) -> String {
    let mut hasher = Sha256::new();
    for entry in entries {
        hasher.update(entry.relative_path.as_bytes());
        hasher.update([0u8]);
        hasher.update(format!("{:o}", entry.mode).as_bytes());
        hasher.update([0u8]);
        hasher.update(entry.bytes.len().to_le_bytes());
        hasher.update(entry.bytes);
        hasher.update([0u8]);
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2 + 7);
    hex.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Hashes every file under `dir`, recursively, in sorted relative-path order.
pub fn hash_dir(dir: &Path) -> io::Result<String> {
    let mut paths = Vec::new();
    collect_files(dir, dir, &mut paths)?;
    paths.sort();

    let mut owned: Vec<(String, u32, Vec<u8>)> = Vec::with_capacity(paths.len());
    for relative in &paths {
        let full = dir.join(relative);
        let bytes = fs::read(&full)?;
        let mode = file_mode(&full)?;
        owned.push((relative.to_string_lossy().replace('\\', "/"), mode, bytes));
    }

    Ok(hash_entries(owned.iter().map(|(path, mode, bytes)| HashedFile {
        relative_path: path.clone(),
        mode: *mode,
        bytes,
    })))
}

/// The digest without its `sha256:` prefix: what a cache directory is keyed by and what a
/// display surface shortens, the prefix being implied wherever either appears.
pub fn short(hash: &str) -> &str {
    hash.rsplit(':').next().unwrap_or(hash)
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<std::path::PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(root, &path, out)?;
        } else if path.is_file() {
            out.push(path.strip_prefix(root).unwrap().to_path_buf());
        }
    }
    Ok(())
}

#[cfg(unix)]
fn file_mode(path: &Path) -> io::Result<u32> {
    use std::os::unix::fs::PermissionsExt;
    Ok(fs::metadata(path)?.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
fn file_mode(_path: &Path) -> io::Result<u32> {
    Ok(0o644)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashing_the_same_tree_twice_is_stable() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"hello").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/b.txt"), b"world").unwrap();

        let first = hash_dir(dir.path()).unwrap();
        let second = hash_dir(dir.path()).unwrap();
        assert_eq!(first, second);
        assert!(first.starts_with("sha256:"));
    }

    #[test]
    fn short_drops_the_algorithm_prefix() {
        assert_eq!(short("sha256:0123456789abcdef"), "0123456789abcdef");
        assert_eq!(short("0123456789abcdef"), "0123456789abcdef");
    }

    #[test]
    fn an_edit_a_rename_or_a_mode_change_all_change_the_hash() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"hello").unwrap();
        let baseline = hash_dir(dir.path()).unwrap();

        fs::write(dir.path().join("a.txt"), b"hello!").unwrap();
        assert_ne!(hash_dir(dir.path()).unwrap(), baseline, "an edit changes the hash");

        fs::write(dir.path().join("a.txt"), b"hello").unwrap();
        fs::rename(dir.path().join("a.txt"), dir.path().join("b.txt")).unwrap();
        assert_ne!(hash_dir(dir.path()).unwrap(), baseline, "a rename changes the hash");

        fs::rename(dir.path().join("b.txt"), dir.path().join("a.txt")).unwrap();
        assert_eq!(hash_dir(dir.path()).unwrap(), baseline, "back to the original, back to the original hash");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir.path().join("a.txt"), fs::Permissions::from_mode(0o755)).unwrap();
            assert_ne!(hash_dir(dir.path()).unwrap(), baseline, "a mode change changes the hash");
        }
    }
}
