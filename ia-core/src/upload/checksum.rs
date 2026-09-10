use std::collections::HashMap;
use std::fmt;
use std::io::Read;
use std::path::Path;

use md5::{Digest, Md5};
use serde::Serialize;
use sha1::Sha1;

/// Hash algorithm used for file verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub enum HashAlgorithm {
    /// MD5 — 32 hex char digest. Default for Internet Archive.
    #[default]
    Md5,
    /// SHA-1 — 40 hex char digest.
    Sha1,
    /// CRC32 — 8 hex char digest (zero-padded).
    Crc32,
}

impl HashAlgorithm {
    /// Returns the metadata field name used by Internet Archive.
    #[must_use]
    pub fn field_name(&self) -> &'static str {
        match self {
            HashAlgorithm::Md5 => "md5",
            HashAlgorithm::Sha1 => "sha1",
            HashAlgorithm::Crc32 => "crc32",
        }
    }

    /// Detect algorithm from hex digest length.
    ///
    /// Returns `None` for 8-char hashes (CRC32 is ambiguous without context)
    /// and for unknown lengths.
    #[must_use]
    pub fn from_hash_len(len: usize) -> Option<Self> {
        match len {
            32 => Some(HashAlgorithm::Md5),
            40 => Some(HashAlgorithm::Sha1),
            _ => None,
        }
    }
}

impl fmt::Display for HashAlgorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.field_name())
    }
}

/// A parsed checksum entry with its hash value and detected algorithm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChecksumEntry {
    /// The hex-encoded hash value.
    pub hash: String,
    /// The algorithm that produced this hash.
    pub algorithm: HashAlgorithm,
}

/// Compute the MD5 hex digest of a file.
pub fn compute_file_md5(path: &Path) -> Result<String, std::io::Error> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Md5::new();
    let mut buffer = [0u8; 1024 * 1024]; // 1 MiB chunks
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Compute the SHA-1 hex digest of a file.
pub fn compute_file_sha1(path: &Path) -> Result<String, std::io::Error> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha1::new();
    let mut buffer = [0u8; 1024 * 1024]; // 1 MiB chunks
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Compute the CRC32 hex digest of a file (zero-padded to 8 chars).
pub fn compute_file_crc32(path: &Path) -> Result<String, std::io::Error> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = crc32fast::Hasher::new();
    let mut buffer = [0u8; 1024 * 1024]; // 1 MiB chunks
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(format!("{:08x}", hasher.finalize()))
}

/// Compute a file hash using the specified algorithm.
pub fn compute_file_hash(path: &Path, algorithm: &HashAlgorithm) -> Result<String, std::io::Error> {
    match algorithm {
        HashAlgorithm::Md5 => compute_file_md5(path),
        HashAlgorithm::Sha1 => compute_file_sha1(path),
        HashAlgorithm::Crc32 => compute_file_crc32(path),
    }
}

/// Async wrapper for [`compute_file_hash`] that runs on a blocking thread.
///
/// Hash computation is CPU + I/O intensive. Running it on tokio's async
/// runtime blocks the executor. This spawns it on the blocking thread pool.
pub async fn compute_file_hash_async(
    path: &std::path::Path,
    algorithm: HashAlgorithm,
) -> std::result::Result<String, std::io::Error> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || compute_file_hash(&path, &algorithm))
        .await
        .map_err(std::io::Error::other)?
}

/// Parse a checksums file (GNU md5sum or BSD md5 format).
///
/// Returns a map of filename to hex MD5 digest.
/// Skips blank lines and unrecognized formats with a warning.
pub fn parse_checksums(content: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Try GNU md5sum format: "hash  filename" or "hash filename"
        if let Some((hash, filename)) = try_parse_gnu(line) {
            map.insert(filename, hash);
            continue;
        }

        // Try BSD format: "MD5 (filename) = hash"
        if let Some((hash, filename)) = try_parse_bsd(line) {
            map.insert(filename, hash);
            continue;
        }

        // Unrecognized line — skip with tracing warning
        tracing::warn!("unrecognized checksums line: {}", line);
    }

    map
}

fn try_parse_gnu(line: &str) -> Option<(String, String)> {
    // "hash  filename" — two spaces, or "hash filename" — one space
    let parts: Vec<&str> = line.splitn(2, char::is_whitespace).collect();
    if parts.len() != 2 {
        return None;
    }
    let hash = parts[0].trim();
    let filename = parts[1].trim();
    // MD5 is 32 hex chars
    if hash.len() == 32 && hash.chars().all(|c| c.is_ascii_hexdigit()) && !filename.is_empty() {
        Some((hash.to_string(), filename.to_string()))
    } else {
        None
    }
}

fn try_parse_bsd(line: &str) -> Option<(String, String)> {
    // "MD5 (filename) = hash"
    let line = line.strip_prefix("MD5 (")?;
    let (filename, rest) = line.split_once(") = ")?;
    let hash = rest.trim();
    if hash.len() == 32 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
        Some((hash.to_string(), filename.to_string()))
    } else {
        None
    }
}

/// Async wrapper for compute_file_md5 that runs on a blocking thread.
///
/// MD5 computation is CPU + I/O intensive. Running it on tokio's async
/// runtime blocks the executor. This spawns it on the blocking thread pool.
pub async fn compute_file_md5_async(
    path: &std::path::Path,
) -> std::result::Result<String, std::io::Error> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || compute_file_md5(&path))
        .await
        .map_err(std::io::Error::other)?
}

/// Parse a checksums file with multi-algorithm support.
///
/// Supports GNU format (`hash  filename`) and BSD format (`ALG (filename) = hash`)
/// for MD5, SHA-1, and CRC32.
///
/// Algorithm detection:
/// - BSD prefixes (`MD5`, `SHA1`, `CRC32`) are always recognized
/// - GNU format: 32-char hex → MD5, 40-char hex → SHA-1
/// - GNU format with 8-char hex is only accepted if `forced_algorithm` is `Some(Crc32)`
/// - If `forced_algorithm` is set, all hashes are interpreted as that type
///
/// Returns a map of filename to [`ChecksumEntry`].
pub fn parse_checksums_multi(
    content: &str,
    forced_algorithm: Option<HashAlgorithm>,
) -> HashMap<String, ChecksumEntry> {
    let mut map = HashMap::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Try BSD format first: "ALG (filename) = hash"
        if let Some((hash, filename, detected)) = try_parse_bsd_multi(line) {
            let algorithm = forced_algorithm.clone().unwrap_or(detected);
            map.insert(filename, ChecksumEntry { hash, algorithm });
            continue;
        }

        // Try GNU format: "hash  filename"
        // Note: try_parse_gnu_multi already applies forced_algorithm internally,
        // so the returned algorithm is already correct.
        if let Some((hash, filename, algorithm)) =
            try_parse_gnu_multi(line, forced_algorithm.as_ref())
        {
            map.insert(filename, ChecksumEntry { hash, algorithm });
            continue;
        }

        // Unrecognized line — skip with tracing warning
        tracing::warn!("unrecognized checksums line: {}", line);
    }

    map
}

/// Try to parse GNU format (`hash  filename`) with multi-algorithm detection.
///
/// Returns `(hash, filename, detected_algorithm)` or `None`.
fn try_parse_gnu_multi(
    line: &str,
    forced_algorithm: Option<&HashAlgorithm>,
) -> Option<(String, String, HashAlgorithm)> {
    let parts: Vec<&str> = line.splitn(2, char::is_whitespace).collect();
    if parts.len() != 2 {
        return None;
    }
    let hash = parts[0].trim();
    let filename = parts[1].trim();
    if filename.is_empty() || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }

    // Determine algorithm from hash length
    let algorithm = if let Some(forced) = forced_algorithm {
        // With a forced algorithm, accept any valid hex hash
        forced.clone()
    } else {
        // Auto-detect from length (8-char CRC32 is ambiguous without force)
        HashAlgorithm::from_hash_len(hash.len())?
    };

    Some((hash.to_string(), filename.to_string(), algorithm))
}

/// Try to parse BSD format with multi-algorithm support.
///
/// Supports `MD5 (file) = hash`, `SHA1 (file) = hash`, `CRC32 (file) = hash`.
/// Returns `(hash, filename, detected_algorithm)` or `None`.
fn try_parse_bsd_multi(line: &str) -> Option<(String, String, HashAlgorithm)> {
    // Try each known prefix
    let (algorithm, expected_len, rest) = if let Some(rest) = line.strip_prefix("MD5 (") {
        (HashAlgorithm::Md5, Some(32), rest)
    } else if let Some(rest) = line.strip_prefix("SHA1 (") {
        (HashAlgorithm::Sha1, Some(40), rest)
    } else {
        let rest = line.strip_prefix("CRC32 (")?;
        (HashAlgorithm::Crc32, Some(8), rest)
    };

    let (filename, hash_part) = rest.split_once(") = ")?;
    let hash = hash_part.trim();

    if let Some(expected) = expected_len {
        if hash.len() != expected {
            return None;
        }
    }
    if !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }

    Some((hash.to_string(), filename.to_string(), algorithm))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn parse_gnu_md5sum_format() {
        let input = "d41d8cd98f00b204e9800998ecf8427e  file.txt\n\
                      abc123def456abc123def456abc123de  other.pdf\n";
        let map = parse_checksums(input);
        assert_eq!(
            map.get("file.txt").unwrap(),
            "d41d8cd98f00b204e9800998ecf8427e"
        );
        assert_eq!(
            map.get("other.pdf").unwrap(),
            "abc123def456abc123def456abc123de"
        );
    }

    #[test]
    fn parse_bsd_md5_format() {
        let input = "MD5 (file.txt) = d41d8cd98f00b204e9800998ecf8427e\n\
                      MD5 (other.pdf) = abc123def456abc123def456abc123de\n";
        let map = parse_checksums(input);
        assert_eq!(
            map.get("file.txt").unwrap(),
            "d41d8cd98f00b204e9800998ecf8427e"
        );
    }

    #[test]
    fn parse_mixed_formats() {
        let input = "d41d8cd98f00b204e9800998ecf8427e  file.txt\n\
                      MD5 (other.pdf) = abc123def456abc123def456abc123de\n";
        let map = parse_checksums(input);
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn parse_skips_blank_lines() {
        let input = "d41d8cd98f00b204e9800998ecf8427e  file.txt\n\n\n";
        let map = parse_checksums(input);
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn compute_md5_of_known_content() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"").unwrap();
        f.flush().unwrap();
        let md5 = compute_file_md5(f.path()).unwrap();
        // MD5 of empty string
        assert_eq!(md5, "d41d8cd98f00b204e9800998ecf8427e");
    }

    #[tokio::test]
    async fn compute_md5_async_matches_sync() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"hello async").unwrap();
        f.flush().unwrap();

        let sync_result = compute_file_md5(f.path()).unwrap();
        let async_result = compute_file_md5_async(f.path()).await.unwrap();
        assert_eq!(sync_result, async_result);
    }

    #[test]
    fn compute_md5_of_hello() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"hello").unwrap();
        f.flush().unwrap();
        let md5 = compute_file_md5(f.path()).unwrap();
        assert_eq!(md5, "5d41402abc4b2a76b9719d911017c592");
    }

    // --- Multi-algorithm hash computation tests ---

    #[test]
    fn compute_sha1_of_empty() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"").unwrap();
        f.flush().unwrap();
        let sha1 = compute_file_sha1(f.path()).unwrap();
        assert_eq!(sha1, "da39a3ee5e6b4b0d3255bfef95601890afd80709");
    }

    #[test]
    fn compute_sha1_of_hello() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"hello").unwrap();
        f.flush().unwrap();
        let sha1 = compute_file_sha1(f.path()).unwrap();
        assert_eq!(sha1, "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d");
    }

    #[test]
    fn compute_crc32_of_empty() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"").unwrap();
        f.flush().unwrap();
        let crc = compute_file_crc32(f.path()).unwrap();
        assert_eq!(crc, "00000000");
    }

    #[test]
    fn compute_crc32_of_hello() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"hello").unwrap();
        f.flush().unwrap();
        let crc = compute_file_crc32(f.path()).unwrap();
        assert_eq!(crc, "3610a686");
    }

    #[test]
    fn compute_file_hash_dispatches_md5() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"hello").unwrap();
        f.flush().unwrap();
        let hash = compute_file_hash(f.path(), &HashAlgorithm::Md5).unwrap();
        assert_eq!(hash, compute_file_md5(f.path()).unwrap());
    }

    #[test]
    fn compute_file_hash_dispatches_sha1() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"hello").unwrap();
        f.flush().unwrap();
        let hash = compute_file_hash(f.path(), &HashAlgorithm::Sha1).unwrap();
        assert_eq!(hash, compute_file_sha1(f.path()).unwrap());
    }

    #[test]
    fn compute_file_hash_dispatches_crc32() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"hello").unwrap();
        f.flush().unwrap();
        let hash = compute_file_hash(f.path(), &HashAlgorithm::Crc32).unwrap();
        assert_eq!(hash, compute_file_crc32(f.path()).unwrap());
    }

    #[tokio::test]
    async fn compute_hash_async_sha1() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"hello").unwrap();
        f.flush().unwrap();
        let async_result = compute_file_hash_async(f.path(), HashAlgorithm::Sha1)
            .await
            .unwrap();
        let sync_result = compute_file_sha1(f.path()).unwrap();
        assert_eq!(async_result, sync_result);
    }

    // --- HashAlgorithm tests ---

    #[test]
    fn hash_algorithm_default_is_md5() {
        assert_eq!(HashAlgorithm::default(), HashAlgorithm::Md5);
    }

    #[test]
    fn hash_algorithm_field_names() {
        assert_eq!(HashAlgorithm::Md5.field_name(), "md5");
        assert_eq!(HashAlgorithm::Sha1.field_name(), "sha1");
        assert_eq!(HashAlgorithm::Crc32.field_name(), "crc32");
    }

    #[test]
    fn hash_algorithm_from_hash_len() {
        assert_eq!(HashAlgorithm::from_hash_len(32), Some(HashAlgorithm::Md5));
        assert_eq!(HashAlgorithm::from_hash_len(40), Some(HashAlgorithm::Sha1));
        assert_eq!(HashAlgorithm::from_hash_len(8), None); // ambiguous
        assert_eq!(HashAlgorithm::from_hash_len(64), None); // unknown
    }

    #[test]
    fn hash_algorithm_display() {
        assert_eq!(format!("{}", HashAlgorithm::Md5), "md5");
        assert_eq!(format!("{}", HashAlgorithm::Sha1), "sha1");
        assert_eq!(format!("{}", HashAlgorithm::Crc32), "crc32");
    }

    // --- parse_checksums_multi tests ---

    #[test]
    fn parse_multi_gnu_md5_auto_detect() {
        let input = "d41d8cd98f00b204e9800998ecf8427e  file.txt\n";
        let map = parse_checksums_multi(input, None);
        let entry = map.get("file.txt").unwrap();
        assert_eq!(entry.hash, "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(entry.algorithm, HashAlgorithm::Md5);
    }

    #[test]
    fn parse_multi_gnu_sha1_auto_detect() {
        let input = "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d  file.txt\n";
        let map = parse_checksums_multi(input, None);
        let entry = map.get("file.txt").unwrap();
        assert_eq!(entry.hash, "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d");
        assert_eq!(entry.algorithm, HashAlgorithm::Sha1);
    }

    #[test]
    fn parse_multi_gnu_8char_without_forced_type_skips() {
        let input = "3610a686  file.txt\n";
        let map = parse_checksums_multi(input, None);
        assert!(map.is_empty());
    }

    #[test]
    fn parse_multi_gnu_8char_with_forced_crc32() {
        let input = "3610a686  file.txt\n";
        let map = parse_checksums_multi(input, Some(HashAlgorithm::Crc32));
        let entry = map.get("file.txt").unwrap();
        assert_eq!(entry.hash, "3610a686");
        assert_eq!(entry.algorithm, HashAlgorithm::Crc32);
    }

    #[test]
    fn parse_multi_bsd_sha1_prefix() {
        let input = "SHA1 (file.txt) = aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d\n";
        let map = parse_checksums_multi(input, None);
        let entry = map.get("file.txt").unwrap();
        assert_eq!(entry.hash, "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d");
        assert_eq!(entry.algorithm, HashAlgorithm::Sha1);
    }

    #[test]
    fn parse_multi_bsd_crc32_prefix() {
        let input = "CRC32 (file.txt) = 3610a686\n";
        let map = parse_checksums_multi(input, None);
        let entry = map.get("file.txt").unwrap();
        assert_eq!(entry.hash, "3610a686");
        assert_eq!(entry.algorithm, HashAlgorithm::Crc32);
    }

    #[test]
    fn parse_multi_bsd_md5_prefix() {
        let input = "MD5 (file.txt) = d41d8cd98f00b204e9800998ecf8427e\n";
        let map = parse_checksums_multi(input, None);
        let entry = map.get("file.txt").unwrap();
        assert_eq!(entry.hash, "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(entry.algorithm, HashAlgorithm::Md5);
    }

    #[test]
    fn parse_multi_forced_algorithm_overrides_detection() {
        // A 32-char hash would normally be auto-detected as MD5,
        // but forced_algorithm overrides it to SHA1 (unusual but valid)
        let input = "d41d8cd98f00b204e9800998ecf8427e  file.txt\n";
        let map = parse_checksums_multi(input, Some(HashAlgorithm::Sha1));
        let entry = map.get("file.txt").unwrap();
        assert_eq!(entry.hash, "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(entry.algorithm, HashAlgorithm::Sha1);
    }

    #[test]
    fn parse_multi_skips_blank_and_unrecognized() {
        let input = "\n\n  \nnot-a-valid-line\nd41d8cd98f00b204e9800998ecf8427e  file.txt\n";
        let map = parse_checksums_multi(input, None);
        assert_eq!(map.len(), 1);
        assert!(map.contains_key("file.txt"));
    }
}
