# `ia verify` Implementation Plan

**Goal:** Implement `ia verify`, a read-only command that asserts local files exist on archive.org with matching checksums and exits non-zero if any file can't be verified.

**Architecture:** New `ia-core/src/verify.rs` module handles verification logic. New `ia-cli/src/commands/verify.rs` handles CLI parsing and output. Extends existing `upload/checksum.rs` with multi-algorithm parsing via a new `parse_checksums_multi()` function. Adds `sha1` and `crc32fast` crates for non-MD5 hash computation.

**Tech Stack:** Rust, clap (CLI), serde (JSON output), sha1/crc32fast (hash), wiremock (tests), assert_cmd (CLI tests)

**Spec:** `docs/plans/2026-03-20-verify-command-design.md`

---

## File Structure

| File | Action | Responsibility |
|------|--------|----------------|
| `ia-core/Cargo.toml` | Modify | Add `sha1`, `crc32fast` dependencies |
| `ia-core/src/verify.rs` | Create | Core verification logic: types, verify_item(), hash dispatch |
| `ia-core/src/upload/checksum.rs` | Modify | Add `parse_checksums_multi()`, SHA-1/CRC32 compute fns |
| `ia-core/src/lib.rs` | Modify | Export `pub mod verify;` |
| `ia-cli/src/commands/verify.rs` | Create | CLI parsing, output formatting, command handler |
| `ia-cli/src/commands/mod.rs` | Modify | Export `pub mod verify;` |
| `ia-cli/src/main.rs` | Modify | Register Verify command + alias + dispatch |
| `ia-cli/tests/verify.rs` | Create | CLI integration tests |

### Design Notes

- **`VerifyOpts.formats` (not `format`)**: Matches `FileFilter.formats` in the
  existing codebase. The spec says `format` but the Vec type naturally pluralizes.
- **Progress callback**: The spec includes `progress: impl Fn(VerifyResult)` in
  `verify_item()`. Deferred to a follow-up — the initial implementation returns
  `Vec<VerifyResult>` which is sufficient for the CLI. Adding a streaming callback
  later is backward-compatible (add an optional parameter or a `verify_item_streaming`
  variant).
- **`ItemMetadata` temporary construction**: `verify_file_against_remote()` wraps
  `remote_files` in a temporary `ItemMetadata` to reuse `files::list()` for
  filtering. This clones the files vec once per `verify_item()` call (not per file).
  Pragmatic trade-off — avoids duplicating filter logic.
- **`format_size()` in verify.rs**: The codebase has `format_bytes()` in
  `tui/widgets.rs`, but it's feature-gated behind `tui`. A minimal local helper
  avoids coupling verify to the TUI feature.

---

## Task 1: Add Hash Dependencies

**Files:**
- Modify: `ia-core/Cargo.toml`

- [ ] **Step 1: Add sha1 and crc32fast to ia-core dependencies**

In `ia-core/Cargo.toml`, add after the `md-5 = "0.10"` line:

```toml
sha1 = "0.10"
crc32fast = "1"
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p ia-core`
Expected: success, no errors

- [ ] **Step 3: Commit**

```bash
git add ia-core/Cargo.toml
git commit -m "deps: add sha1 and crc32fast crates for multi-algorithm hashing"
```

---

## Task 2: Extend Checksum Module — Multi-Algorithm Parsing and Computation

**Files:**
- Modify: `ia-core/src/upload/checksum.rs`

This task adds SHA-1/CRC32 hash computation functions, the `HashAlgorithm` enum, and a new `parse_checksums_multi()` parser that preserves backward compatibility with the existing `parse_checksums()`.

- [ ] **Step 1: Write tests for HashAlgorithm and new hash computation functions**

Add to the bottom of `ia-core/src/upload/checksum.rs`, inside the existing `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn compute_sha1_of_empty() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"").unwrap();
        f.flush().unwrap();
        let hash = compute_file_sha1(f.path()).unwrap();
        assert_eq!(hash, "da39a3ee5e6b4b0d3255bfef95601890afd80709");
    }

    #[test]
    fn compute_sha1_of_hello() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"hello").unwrap();
        f.flush().unwrap();
        let hash = compute_file_sha1(f.path()).unwrap();
        assert_eq!(hash, "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d");
    }

    #[test]
    fn compute_crc32_of_empty() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"").unwrap();
        f.flush().unwrap();
        let hash = compute_file_crc32(f.path()).unwrap();
        assert_eq!(hash, "00000000");
    }

    #[test]
    fn compute_crc32_of_hello() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"hello").unwrap();
        f.flush().unwrap();
        let hash = compute_file_crc32(f.path()).unwrap();
        assert_eq!(hash, "3610a686");
    }

    #[test]
    fn compute_file_hash_dispatches_md5() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"hello").unwrap();
        f.flush().unwrap();
        let hash = compute_file_hash(f.path(), &HashAlgorithm::Md5).unwrap();
        assert_eq!(hash, "5d41402abc4b2a76b9719d911017c592");
    }

    #[test]
    fn compute_file_hash_dispatches_sha1() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"hello").unwrap();
        f.flush().unwrap();
        let hash = compute_file_hash(f.path(), &HashAlgorithm::Sha1).unwrap();
        assert_eq!(hash, "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d");
    }

    #[test]
    fn compute_file_hash_dispatches_crc32() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"hello").unwrap();
        f.flush().unwrap();
        let hash = compute_file_hash(f.path(), &HashAlgorithm::Crc32).unwrap();
        assert_eq!(hash, "3610a686");
    }

    #[tokio::test]
    async fn compute_hash_async_sha1() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"hello").unwrap();
        f.flush().unwrap();
        let hash = compute_file_hash_async(f.path(), &HashAlgorithm::Sha1)
            .await
            .unwrap();
        assert_eq!(hash, "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-core -- checksum`
Expected: compilation errors (functions don't exist yet)

- [ ] **Step 3: Implement HashAlgorithm enum and hash computation functions**

Add these items to `ia-core/src/upload/checksum.rs`, above the existing `compute_file_md5` function:

```rust
use serde::Serialize;
use sha1::Sha1;

/// Supported hash algorithms for verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum HashAlgorithm {
    #[default]
    Md5,
    Sha1,
    Crc32,
}

impl HashAlgorithm {
    /// The field name in IA's FileMetadata JSON for this algorithm.
    pub fn field_name(&self) -> &'static str {
        match self {
            HashAlgorithm::Md5 => "md5",
            HashAlgorithm::Sha1 => "sha1",
            HashAlgorithm::Crc32 => "crc32",
        }
    }

    /// Detect algorithm from hex hash length.
    /// Returns None for ambiguous lengths (CRC32 = 8 chars is ambiguous in GNU format).
    pub fn from_hash_len(len: usize) -> Option<Self> {
        match len {
            32 => Some(HashAlgorithm::Md5),
            40 => Some(HashAlgorithm::Sha1),
            _ => None,
        }
    }
}

impl std::fmt::Display for HashAlgorithm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.field_name())
    }
}
```

Add after `compute_file_md5`:

```rust
/// Compute the SHA-1 hex digest of a file.
pub fn compute_file_sha1(path: &Path) -> Result<String, std::io::Error> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha1::new();
    let mut buffer = [0u8; 1024 * 1024];
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
    let mut buffer = [0u8; 1024 * 1024];
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

/// Async wrapper for compute_file_hash that runs on a blocking thread.
pub async fn compute_file_hash_async(
    path: &std::path::Path,
    algorithm: &HashAlgorithm,
) -> std::result::Result<String, std::io::Error> {
    let path = path.to_path_buf();
    let algorithm = algorithm.clone();
    tokio::task::spawn_blocking(move || compute_file_hash(&path, &algorithm))
        .await
        .map_err(std::io::Error::other)?
}
```

Update the imports at the top of the file to include:

```rust
use serde::Serialize;
use sha1::Sha1;
```

And update the existing `use md5::{Digest, Md5};` to:

```rust
use md5::{Digest, Md5};
```

Note: Both `md5::Digest` and `sha1::Digest` come from the same `digest` crate, so `Digest` trait is shared. Import `Sha1` directly and use `Digest` from `md5`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-core -- checksum`
Expected: all checksum tests pass

- [ ] **Step 5: Write tests for parse_checksums_multi**

Add to the test module:

```rust
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
        let input = "da39a3ee5e6b4b0d3255bfef95601890afd80709  file.txt\n";
        let map = parse_checksums_multi(input, None);
        let entry = map.get("file.txt").unwrap();
        assert_eq!(entry.hash, "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(entry.algorithm, HashAlgorithm::Sha1);
    }

    #[test]
    fn parse_multi_gnu_8char_without_forced_type_skips() {
        // 8 hex chars in GNU format is ambiguous — skip without --checksum-type
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
        let input = "SHA1 (file.txt) = da39a3ee5e6b4b0d3255bfef95601890afd80709\n";
        let map = parse_checksums_multi(input, None);
        let entry = map.get("file.txt").unwrap();
        assert_eq!(entry.algorithm, HashAlgorithm::Sha1);
    }

    #[test]
    fn parse_multi_bsd_crc32_prefix() {
        let input = "CRC32 (file.txt) = 3610a686\n";
        let map = parse_checksums_multi(input, None);
        let entry = map.get("file.txt").unwrap();
        assert_eq!(entry.algorithm, HashAlgorithm::Crc32);
    }

    #[test]
    fn parse_multi_bsd_md5_prefix() {
        let input = "MD5 (file.txt) = d41d8cd98f00b204e9800998ecf8427e\n";
        let map = parse_checksums_multi(input, None);
        let entry = map.get("file.txt").unwrap();
        assert_eq!(entry.algorithm, HashAlgorithm::Md5);
    }

    #[test]
    fn parse_multi_forced_algorithm_overrides_detection() {
        // This is a valid MD5 hash, but forced to SHA1 — trust the user
        let input = "d41d8cd98f00b204e9800998ecf8427e  file.txt\n";
        let map = parse_checksums_multi(input, Some(HashAlgorithm::Sha1));
        let entry = map.get("file.txt").unwrap();
        assert_eq!(entry.algorithm, HashAlgorithm::Sha1);
    }

    #[test]
    fn parse_multi_skips_blank_and_unrecognized() {
        let input = "\n\nbadline\nd41d8cd98f00b204e9800998ecf8427e  file.txt\n";
        let map = parse_checksums_multi(input, None);
        assert_eq!(map.len(), 1);
    }
```

- [ ] **Step 6: Run tests to verify they fail**

Run: `cargo test -p ia-core -- parse_multi`
Expected: compilation errors

- [ ] **Step 7: Implement parse_checksums_multi and ChecksumEntry**

Add to `ia-core/src/upload/checksum.rs`:

```rust
/// Parsed checksum entry with detected algorithm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChecksumEntry {
    pub hash: String,
    pub algorithm: HashAlgorithm,
}

/// Parse a checksums file with multi-algorithm support.
///
/// Auto-detects algorithm from hash length (MD5=32, SHA-1=40) or BSD prefix.
/// CRC32 (8 chars) in GNU format requires `forced_algorithm = Some(Crc32)`.
/// If `forced_algorithm` is set, all hashes are interpreted as that type.
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

        // Try BSD format with algorithm prefix: "ALG (filename) = hash"
        if let Some((hash, filename, alg)) = try_parse_bsd_multi(line) {
            let algorithm = forced_algorithm.clone().unwrap_or(alg);
            map.insert(filename, ChecksumEntry { hash, algorithm });
            continue;
        }

        // Try GNU format: "hash  filename" or "hash filename"
        if let Some((hash, filename)) = try_parse_gnu_multi(line, &forced_algorithm) {
            let algorithm = forced_algorithm
                .clone()
                .or_else(|| HashAlgorithm::from_hash_len(hash.len()))
                .unwrap_or(HashAlgorithm::Md5);
            map.insert(filename, ChecksumEntry { hash, algorithm });
            continue;
        }

        tracing::warn!("unrecognized checksums line: {}", line);
    }

    map
}

fn try_parse_bsd_multi(line: &str) -> Option<(String, String, HashAlgorithm)> {
    // "MD5 (filename) = hash", "SHA1 (filename) = hash", "CRC32 (filename) = hash"
    let (alg, rest) = if let Some(r) = line.strip_prefix("MD5 (") {
        (HashAlgorithm::Md5, r)
    } else if let Some(r) = line.strip_prefix("SHA1 (") {
        (HashAlgorithm::Sha1, r)
    } else if let Some(r) = line.strip_prefix("CRC32 (") {
        (HashAlgorithm::Crc32, r)
    } else {
        return None;
    };
    let (filename, hash_part) = rest.split_once(") = ")?;
    let hash = hash_part.trim();
    if !hash.is_empty() && hash.chars().all(|c| c.is_ascii_hexdigit()) {
        Some((hash.to_string(), filename.to_string(), alg))
    } else {
        None
    }
}

fn try_parse_gnu_multi(
    line: &str,
    forced: &Option<HashAlgorithm>,
) -> Option<(String, String)> {
    let parts: Vec<&str> = line.splitn(2, char::is_whitespace).collect();
    if parts.len() != 2 {
        return None;
    }
    let hash = parts[0].trim();
    let filename = parts[1].trim();
    if filename.is_empty() || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }

    // Validate hash length
    match hash.len() {
        32 | 40 => Some((hash.to_string(), filename.to_string())),
        8 if forced.is_some() => Some((hash.to_string(), filename.to_string())),
        _ => None,
    }
}
```

- [ ] **Step 8: Run tests to verify they pass**

Run: `cargo test -p ia-core -- checksum`
Expected: all tests pass

- [ ] **Step 9: Commit**

```bash
git add ia-core/src/upload/checksum.rs
git commit -m "feat(verify): add multi-algorithm hash computation and checksum parsing

Add SHA-1 and CRC32 hash computation alongside existing MD5.
Add HashAlgorithm enum, compute_file_hash() dispatcher, and
parse_checksums_multi() for auto-detecting algorithm from
checksum files (GNU/BSD formats).

Existing parse_checksums() unchanged for backward compatibility."
```

---

## Task 3: Core Verify Module — Types and verify_item()

**Files:**
- Create: `ia-core/src/verify.rs`
- Modify: `ia-core/src/lib.rs`

- [ ] **Step 1: Write tests for core verification logic**

Create `ia-core/src/verify.rs` with tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FileMetadata, ItemMetadata, MetadataFields};
    use std::collections::HashMap as StdHashMap;

    fn make_item(files: Vec<FileMetadata>) -> ItemMetadata {
        ItemMetadata {
            metadata: MetadataFields::default(),
            files,
            server: None,
            d1: None,
            d2: None,
            dir: None,
            files_count: None,
            item_size: None,
            is_dark: false,
        }
    }

    fn make_remote_file(name: &str, md5: &str, sha1: &str, crc32: &str) -> FileMetadata {
        FileMetadata {
            name: name.to_string(),
            source: Some("original".to_string()),
            format: Some("Data".to_string()),
            md5: Some(md5.to_string()),
            sha1: Some(sha1.to_string()),
            crc32: Some(crc32.to_string()),
            size: Some(100),
            mtime: None,
            original: None,
            rotation: None,
            extra: StdHashMap::new(),
        }
    }

    // ─── Hash-only matching ──────────────────────────────────────────

    #[test]
    fn verify_hash_only_exact_name_match() {
        let remote_files = vec![make_remote_file("file.txt", "abc123", "sha111", "crc111")];
        let opts = VerifyOpts::default();
        let result = verify_file_against_remote(
            "test-item",
            "file.txt",
            "abc123",
            None,
            &remote_files,
            &opts,
        );
        assert_eq!(result.status, VerifyStatus::Verified);
        assert_eq!(result.remote_key.as_deref(), Some("file.txt"));
    }

    #[test]
    fn verify_hash_only_different_name_match() {
        let remote_files = vec![make_remote_file("renamed.txt", "abc123", "sha111", "crc111")];
        let opts = VerifyOpts::default();
        let result = verify_file_against_remote(
            "test-item",
            "file.txt",
            "abc123",
            None,
            &remote_files,
            &opts,
        );
        assert_eq!(result.status, VerifyStatus::Verified);
        assert_eq!(result.remote_key.as_deref(), Some("renamed.txt"));
    }

    #[test]
    fn verify_hash_only_no_match() {
        let remote_files = vec![make_remote_file("file.txt", "different", "sha111", "crc111")];
        let opts = VerifyOpts::default();
        let result = verify_file_against_remote(
            "test-item",
            "file.txt",
            "abc123",
            None,
            &remote_files,
            &opts,
        );
        assert_eq!(result.status, VerifyStatus::Missing);
        assert!(result.remote_key.is_none());
    }

    #[test]
    fn verify_hash_only_sha1() {
        let remote_files = vec![make_remote_file("file.txt", "md5xxx", "sha111", "crc111")];
        let opts = VerifyOpts {
            algorithm: HashAlgorithm::Sha1,
            ..Default::default()
        };
        let result = verify_file_against_remote(
            "test-item",
            "file.txt",
            "sha111",
            None,
            &remote_files,
            &opts,
        );
        assert_eq!(result.status, VerifyStatus::Verified);
    }

    #[test]
    fn verify_hash_only_crc32() {
        let remote_files = vec![make_remote_file("file.txt", "md5xxx", "sha111", "crc111")];
        let opts = VerifyOpts {
            algorithm: HashAlgorithm::Crc32,
            ..Default::default()
        };
        let result = verify_file_against_remote(
            "test-item",
            "file.txt",
            "crc111",
            None,
            &remote_files,
            &opts,
        );
        assert_eq!(result.status, VerifyStatus::Verified);
    }

    // ─── --match-names mode ──────────────────────────────────────────

    #[test]
    fn verify_match_names_exact_match() {
        let remote_files = vec![make_remote_file("file.txt", "abc123", "sha111", "crc111")];
        let opts = VerifyOpts {
            match_names: true,
            ..Default::default()
        };
        let result = verify_file_against_remote(
            "test-item",
            "file.txt",
            "abc123",
            None,
            &remote_files,
            &opts,
        );
        assert_eq!(result.status, VerifyStatus::Verified);
    }

    #[test]
    fn verify_match_names_hash_differs() {
        let remote_files = vec![make_remote_file("file.txt", "different", "sha111", "crc111")];
        let opts = VerifyOpts {
            match_names: true,
            ..Default::default()
        };
        let result = verify_file_against_remote(
            "test-item",
            "file.txt",
            "abc123",
            None,
            &remote_files,
            &opts,
        );
        assert_eq!(result.status, VerifyStatus::Mismatch);
        assert_eq!(result.remote_hash.as_deref(), Some("different"));
    }

    #[test]
    fn verify_match_names_file_not_found() {
        let remote_files = vec![make_remote_file("other.txt", "abc123", "sha111", "crc111")];
        let opts = VerifyOpts {
            match_names: true,
            ..Default::default()
        };
        let result = verify_file_against_remote(
            "test-item",
            "file.txt",
            "abc123",
            None,
            &remote_files,
            &opts,
        );
        assert_eq!(result.status, VerifyStatus::Missing);
    }

    #[test]
    fn verify_match_names_remote_has_no_hash() {
        let mut f = make_remote_file("file.txt", "", "", "");
        f.md5 = None;
        f.sha1 = None;
        f.crc32 = None;
        let remote_files = vec![f];
        let opts = VerifyOpts {
            match_names: true,
            ..Default::default()
        };
        let result = verify_file_against_remote(
            "test-item",
            "file.txt",
            "abc123",
            None,
            &remote_files,
            &opts,
        );
        assert_eq!(result.status, VerifyStatus::Error);
    }

    // ─── File filtering ──────────────────────────────────────────────

    #[test]
    fn verify_with_glob_filter() {
        let remote_files = vec![
            make_remote_file("file.txt", "abc123", "sha111", "crc111"),
            make_remote_file("file.pdf", "abc123", "sha111", "crc111"),
        ];
        let opts = VerifyOpts {
            glob: Some("*.pdf".to_string()),
            ..Default::default()
        };
        // Hash matches file.pdf (which passes glob) but not file.txt (filtered out)
        let result = verify_file_against_remote(
            "test-item",
            "local.txt",
            "abc123",
            None,
            &remote_files,
            &opts,
        );
        assert_eq!(result.status, VerifyStatus::Verified);
        assert_eq!(result.remote_key.as_deref(), Some("file.pdf"));
    }

    // ─── Empty item ──────────────────────────────────────────────────

    #[test]
    fn verify_empty_item_reports_missing() {
        let remote_files: Vec<FileMetadata> = vec![];
        let opts = VerifyOpts::default();
        let result = verify_file_against_remote(
            "test-item",
            "file.txt",
            "abc123",
            None,
            &remote_files,
            &opts,
        );
        assert_eq!(result.status, VerifyStatus::Missing);
    }

    // ─── Multiple remote files with same hash ────────────────────────

    #[test]
    fn verify_first_matching_remote_file_wins() {
        let remote_files = vec![
            make_remote_file("first.txt", "abc123", "sha111", "crc111"),
            make_remote_file("second.txt", "abc123", "sha111", "crc111"),
        ];
        let opts = VerifyOpts::default();
        let result = verify_file_against_remote(
            "test-item",
            "local.txt",
            "abc123",
            None,
            &remote_files,
            &opts,
        );
        assert_eq!(result.status, VerifyStatus::Verified);
        assert_eq!(result.remote_key.as_deref(), Some("first.txt"));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-core -- verify`
Expected: compilation errors (module doesn't exist yet)

- [ ] **Step 3: Add `pub mod verify;` to lib.rs**

In `ia-core/src/lib.rs`, add after the `pub mod upload;` line:

```rust
pub mod verify;
```

- [ ] **Step 4: Implement verify types and verify_file_against_remote()**

Create the main body of `ia-core/src/verify.rs` (above the test module):

```rust
use std::collections::HashMap;
use std::path::PathBuf;

use serde::Serialize;

use crate::files::{self, FileFilter};
use crate::types::{FileMetadata, FileSource};
use crate::upload::checksum::HashAlgorithm;
use crate::IaClient;

/// Result of verifying a single file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerifyResult {
    pub identifier: String,
    pub local_file: String,
    pub status: VerifyStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_hash: Option<String>,
    pub algorithm: String,
    /// Local file size in bytes (None when verifying from pre-computed hash).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// Error detail (only set when status is Error).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Status of a single file verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyStatus {
    Verified,
    Missing,
    Mismatch,
    Error,
}

/// Options for verify operations.
#[derive(Debug, Clone)]
pub struct VerifyOpts {
    /// Hash algorithm to use.
    pub algorithm: HashAlgorithm,
    /// Pre-computed checksums keyed by filename.
    pub checksums: Option<HashMap<String, String>>,
    /// Require filename match (not just hash).
    pub match_names: bool,
    /// Filter remote files by glob pattern.
    pub glob: Option<String>,
    /// Filter remote files by format.
    pub formats: Vec<String>,
    /// Filter remote files by source.
    pub source: Option<FileSource>,
}

impl Default for VerifyOpts {
    fn default() -> Self {
        Self {
            algorithm: HashAlgorithm::Md5,
            checksums: None,
            match_names: false,
            glob: None,
            formats: Vec::new(),
            source: None,
        }
    }
}

/// Input for verification — either a local file path or a pre-computed hash.
#[derive(Debug, Clone)]
pub enum VerifyInput {
    /// Local file to hash and verify.
    LocalFile(PathBuf),
    /// Pre-computed hash (from --checksum-file or spreadsheet column).
    Hash { filename: String, hash: String },
}

/// Extract the hash for a specific algorithm from a remote FileMetadata entry.
fn get_remote_hash(file: &FileMetadata, algorithm: &HashAlgorithm) -> Option<&str> {
    match algorithm {
        HashAlgorithm::Md5 => file.md5.as_deref(),
        HashAlgorithm::Sha1 => file.sha1.as_deref(),
        HashAlgorithm::Crc32 => file.crc32.as_deref(),
    }
}

/// Verify a single file against a list of remote files.
///
/// This is the core matching logic, separated from I/O for testability.
pub fn verify_file_against_remote(
    identifier: &str,
    local_filename: &str,
    local_hash: &str,
    local_bytes: Option<u64>,
    remote_files: &[FileMetadata],
    opts: &VerifyOpts,
) -> VerifyResult {
    let algorithm_str = opts.algorithm.to_string();

    // Apply file filters to remote files
    let filter = FileFilter {
        glob: opts.glob.clone(),
        formats: opts.formats.clone(),
        source: opts.source.clone(),
        ..Default::default()
    };

    // Build a temporary ItemMetadata just for filtering
    // (files::list needs &ItemMetadata, but we only have the files vec)
    let temp_item = crate::types::ItemMetadata {
        metadata: crate::types::MetadataFields::default(),
        files: remote_files.to_vec(),
        server: None,
        d1: None,
        d2: None,
        dir: None,
        files_count: None,
        item_size: None,
        is_dark: false,
    };
    let filtered: Vec<&FileMetadata> = files::list(&temp_item, &filter);

    if opts.match_names {
        // Find remote file by name
        match filtered.iter().find(|f| f.name == local_filename) {
            Some(remote) => {
                match get_remote_hash(remote, &opts.algorithm) {
                    Some(remote_hash) if remote_hash == local_hash => VerifyResult {
                        identifier: identifier.to_string(),
                        local_file: local_filename.to_string(),
                        status: VerifyStatus::Verified,
                        remote_key: Some(remote.name.clone()),
                        local_hash: Some(local_hash.to_string()),
                        remote_hash: Some(remote_hash.to_string()),
                        algorithm: algorithm_str,
                        bytes: local_bytes,
                        detail: None,
                    },
                    Some(remote_hash) => VerifyResult {
                        identifier: identifier.to_string(),
                        local_file: local_filename.to_string(),
                        status: VerifyStatus::Mismatch,
                        remote_key: Some(remote.name.clone()),
                        local_hash: Some(local_hash.to_string()),
                        remote_hash: Some(remote_hash.to_string()),
                        algorithm: algorithm_str,
                        bytes: local_bytes,
                        detail: None,
                    },
                    None => VerifyResult {
                        identifier: identifier.to_string(),
                        local_file: local_filename.to_string(),
                        status: VerifyStatus::Error,
                        remote_key: Some(remote.name.clone()),
                        local_hash: Some(local_hash.to_string()),
                        remote_hash: None,
                        algorithm: algorithm_str,
                        bytes: local_bytes,
                        detail: Some(format!(
                            "remote file has no {} hash",
                            opts.algorithm
                        )),
                    },
                }
            }
            None => VerifyResult {
                identifier: identifier.to_string(),
                local_file: local_filename.to_string(),
                status: VerifyStatus::Missing,
                remote_key: None,
                local_hash: Some(local_hash.to_string()),
                remote_hash: None,
                algorithm: algorithm_str,
                bytes: local_bytes,
                detail: None,
            },
        }
    } else {
        // Hash-only mode: find any remote file with matching hash
        match filtered
            .iter()
            .find(|f| get_remote_hash(f, &opts.algorithm) == Some(local_hash))
        {
            Some(remote) => VerifyResult {
                identifier: identifier.to_string(),
                local_file: local_filename.to_string(),
                status: VerifyStatus::Verified,
                remote_key: Some(remote.name.clone()),
                local_hash: Some(local_hash.to_string()),
                remote_hash: get_remote_hash(remote, &opts.algorithm)
                    .map(|s| s.to_string()),
                algorithm: algorithm_str,
                bytes: local_bytes,
                detail: None,
            },
            None => VerifyResult {
                identifier: identifier.to_string(),
                local_file: local_filename.to_string(),
                status: VerifyStatus::Missing,
                remote_key: None,
                local_hash: Some(local_hash.to_string()),
                remote_hash: None,
                algorithm: algorithm_str,
                bytes: local_bytes,
                detail: None,
            },
        }
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p ia-core -- verify`
Expected: all verify tests pass

- [ ] **Step 6: Commit**

```bash
git add ia-core/src/verify.rs ia-core/src/lib.rs
git commit -m "feat(verify): add core verification types and matching logic

Add verify_file_against_remote() with two modes:
- Hash-only (default): match any remote file by hash
- --match-names: require both filename and hash to match

Statuses: Verified, Missing, Mismatch (match-names only), Error.
Supports md5/sha1/crc32 via HashAlgorithm, glob/format/source
filtering of remote files."
```

---

## Task 4: Core Verify Module — verify_item() with IaClient

**Files:**
- Modify: `ia-core/src/verify.rs`

- [ ] **Step 1: Write integration tests using wiremock**

Add to `ia-core/src/verify.rs` test module:

```rust
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_test_client(host: &str) -> IaClient {
        let mut config = crate::config::IaConfig::default();
        config.general.host = host.to_string();
        config.general.secure = false;
        config.s3.access = Some("test_access".to_string());
        config.s3.secret = Some("test_secret".to_string());
        IaClient::from_config(config).unwrap()
    }

    #[tokio::test]
    async fn verify_item_all_verified() {
        let server = MockServer::start().await;
        let client = make_test_client(&server.uri().replace("http://", ""));

        // Create temp files
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("hello.txt");
        std::fs::write(&file_path, b"hello").unwrap();
        // MD5 of "hello" = 5d41402abc4b2a76b9719d911017c592

        Mock::given(method("GET"))
            .and(path("/metadata/test-item"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "metadata": {"identifier": "test-item"},
                "files": [{
                    "name": "hello.txt",
                    "source": "original",
                    "format": "Data",
                    "md5": "5d41402abc4b2a76b9719d911017c592",
                    "sha1": "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d",
                    "crc32": "3610a686",
                    "size": "5"
                }]
            })))
            .mount(&server)
            .await;

        let inputs = vec![VerifyInput::LocalFile(file_path)];
        let opts = VerifyOpts::default();
        let results = verify_item(&client, "test-item", &inputs, &opts).await.unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, VerifyStatus::Verified);
        assert_eq!(results[0].remote_key.as_deref(), Some("hello.txt"));
    }

    #[tokio::test]
    async fn verify_item_hash_precomputed() {
        let server = MockServer::start().await;
        let client = make_test_client(&server.uri().replace("http://", ""));

        Mock::given(method("GET"))
            .and(path("/metadata/test-item"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "metadata": {"identifier": "test-item"},
                "files": [{
                    "name": "file.txt",
                    "md5": "abc123",
                    "size": "100"
                }]
            })))
            .mount(&server)
            .await;

        let inputs = vec![VerifyInput::Hash {
            filename: "file.txt".to_string(),
            hash: "abc123".to_string(),
        }];
        let opts = VerifyOpts::default();
        let results = verify_item(&client, "test-item", &inputs, &opts).await.unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, VerifyStatus::Verified);
    }

    #[tokio::test]
    async fn verify_item_missing_file() {
        let server = MockServer::start().await;
        let client = make_test_client(&server.uri().replace("http://", ""));

        Mock::given(method("GET"))
            .and(path("/metadata/test-item"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "metadata": {"identifier": "test-item"},
                "files": [{
                    "name": "other.txt",
                    "md5": "different_hash",
                    "size": "100"
                }]
            })))
            .mount(&server)
            .await;

        let inputs = vec![VerifyInput::Hash {
            filename: "file.txt".to_string(),
            hash: "abc123".to_string(),
        }];
        let opts = VerifyOpts::default();
        let results = verify_item(&client, "test-item", &inputs, &opts).await.unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, VerifyStatus::Missing);
    }

    #[tokio::test]
    async fn verify_item_404_reports_all_missing() {
        let server = MockServer::start().await;
        let client = make_test_client(&server.uri().replace("http://", ""));

        Mock::given(method("GET"))
            .and(path("/metadata/nonexistent"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "metadata": {},
                "files": []
            })))
            .mount(&server)
            .await;

        let inputs = vec![VerifyInput::Hash {
            filename: "file.txt".to_string(),
            hash: "abc123".to_string(),
        }];
        let opts = VerifyOpts::default();
        let results = verify_item(&client, "nonexistent", &inputs, &opts)
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, VerifyStatus::Missing);
    }

    #[tokio::test]
    async fn verify_item_mixed_results() {
        let server = MockServer::start().await;
        let client = make_test_client(&server.uri().replace("http://", ""));

        Mock::given(method("GET"))
            .and(path("/metadata/test-item"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "metadata": {"identifier": "test-item"},
                "files": [{
                    "name": "exists.txt",
                    "md5": "hash_a",
                    "size": "100"
                }]
            })))
            .mount(&server)
            .await;

        let inputs = vec![
            VerifyInput::Hash {
                filename: "exists.txt".to_string(),
                hash: "hash_a".to_string(),
            },
            VerifyInput::Hash {
                filename: "missing.txt".to_string(),
                hash: "hash_b".to_string(),
            },
        ];
        let opts = VerifyOpts::default();
        let results = verify_item(&client, "test-item", &inputs, &opts).await.unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].status, VerifyStatus::Verified);
        assert_eq!(results[1].status, VerifyStatus::Missing);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-core -- verify_item`
Expected: compilation error (verify_item function doesn't exist)

- [ ] **Step 3: Implement verify_item()**

Add to `ia-core/src/verify.rs`, after `verify_file_against_remote()`:

```rust
/// Verify files in a single item against its remote metadata.
///
/// Fetches item metadata once, then checks each input file against the
/// remote file list. Returns one VerifyResult per input file.
pub async fn verify_item(
    client: &IaClient,
    identifier: &str,
    inputs: &[VerifyInput],
    opts: &VerifyOpts,
) -> crate::Result<Vec<VerifyResult>> {
    use crate::upload::checksum::compute_file_hash_async;

    // Fetch remote metadata
    let item = client.get_item(identifier).await?;

    let mut results = Vec::with_capacity(inputs.len());

    for input in inputs {
        let result = match input {
            VerifyInput::LocalFile(path) => {
                let filename = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();

                // If we have pre-computed checksums, use them
                let hash = if let Some(checksums) = &opts.checksums {
                    if let Some(h) = checksums.get(&filename) {
                        h.clone()
                    } else {
                        // Compute hash from local file
                        match compute_file_hash_async(path, &opts.algorithm).await {
                            Ok(h) => h,
                            Err(e) => {
                                results.push(VerifyResult {
                                    identifier: identifier.to_string(),
                                    local_file: filename,
                                    status: VerifyStatus::Error,
                                    remote_key: None,
                                    local_hash: None,
                                    remote_hash: None,
                                    algorithm: opts.algorithm.to_string(),
                                    bytes: None,
                                    detail: Some(e.to_string()),
                                });
                                continue;
                            }
                        }
                    }
                } else {
                    match compute_file_hash_async(path, &opts.algorithm).await {
                        Ok(h) => h,
                        Err(e) => {
                            results.push(VerifyResult {
                                identifier: identifier.to_string(),
                                local_file: filename,
                                status: VerifyStatus::Error,
                                remote_key: None,
                                local_hash: None,
                                remote_hash: None,
                                algorithm: opts.algorithm.to_string(),
                                bytes: None,
                                detail: Some(e.to_string()),
                            });
                            continue;
                        }
                    }
                };

                let bytes = std::fs::metadata(path).ok().map(|m| m.len());
                verify_file_against_remote(
                    identifier,
                    &filename,
                    &hash,
                    bytes,
                    &item.files,
                    opts,
                )
            }
            VerifyInput::Hash { filename, hash } => {
                verify_file_against_remote(
                    identifier,
                    filename,
                    hash,
                    None,
                    &item.files,
                    opts,
                )
            }
        };
        results.push(result);
    }

    Ok(results)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-core -- verify`
Expected: all verify tests pass

- [ ] **Step 5: Commit**

```bash
git add ia-core/src/verify.rs
git commit -m "feat(verify): add verify_item() with wiremock integration tests

Fetches item metadata once, then verifies each input file against
remote files. Supports both local files (hashed on blocking pool)
and pre-computed hashes. Five integration tests with wiremock."
```

---

## Task 5: CLI Command — Argument Parsing and Registration

**Files:**
- Create: `ia-cli/src/commands/verify.rs`
- Modify: `ia-cli/src/commands/mod.rs`
- Modify: `ia-cli/src/main.rs`

- [ ] **Step 1: Create verify CLI module with clap structs**

Create `ia-cli/src/commands/verify.rs`:

```rust
use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use color_print::cstr;

use ia_core::types::FileSource;
use ia_core::upload::checksum::HashAlgorithm;
use ia_core::IaClient;

/// Verify that local files exist on archive.org with matching checksums.
///
/// Exits with code 0 if all files are verified, 1 if any file is missing
/// or mismatched. Designed for automation pipelines.
#[derive(Debug, Args)]
#[command(
    about = "Verify local files exist on archive.org with matching checksums",
    long_about = "Verify that local files exist on archive.org with matching checksums.\n\
        Exits with code 0 if all files are verified, 1 if any file is missing or mismatched.\n\
        Designed for automation pipelines where downstream steps must only run after\n\
        upload integrity is confirmed.",
    after_long_help = cstr!(
        "<bold><underline>Examples:</underline></bold>\n\
         \n  <dim># Verify specific files were uploaded</dim>\
         \n  <bold>$ ia verify my-item file1.pdf file2.pdf</bold>\
         \n\n  <dim># Verify an entire directory</dim>\
         \n  <bold>$ ia verify my-item ./local-files/</bold>\
         \n\n  <dim># Verify using pre-computed checksums (no local files needed)</dim>\
         \n  <bold>$ ia verify my-item --checksum-file md5sums.txt</bold>\
         \n\n  <dim># Use SHA-1 instead of MD5</dim>\
         \n  <bold>$ ia verify my-item ./files/ --checksum-type sha1</bold>\
         \n\n  <dim># Require exact filename match</dim>\
         \n  <bold>$ ia verify my-item ./files/ --match-names</bold>\
         \n\n  <dim># Only verify PDFs on the remote item</dim>\
         \n  <bold>$ ia verify my-item ./files/ --glob '*.pdf'</bold>\
         \n\n  <dim># Batch verify from upload spreadsheet</dim>\
         \n  <bold>$ ia verify --spreadsheet upload.csv</bold>\
         \n\n  <dim># Gate a script on successful verification</dim>\
         \n  <bold>$ ia verify my-item ./files/ -q && ./post-upload.sh</bold>\
         \n\n  <dim># Generate checksums, upload, then verify without re-hashing</dim>\
         \n  <bold>$ md5sum ./files/* > checksums.txt</bold>\
         \n  <bold>$ ia upload my-item ./files/</bold>\
         \n  <bold>$ ia verify my-item --checksum-file checksums.txt</bold>\n"
    ),
)]
pub struct VerifyArgs {
    /// Item identifier to verify against
    #[arg(required_unless_present = "spreadsheet")]
    pub identifier: Option<String>,

    /// Local files or directories to verify
    #[arg(required_unless_present_any = ["checksum_file", "spreadsheet"])]
    pub files: Vec<PathBuf>,

    /// Pre-computed checksum file (GNU md5sum/sha1sum/BSD formats)
    #[arg(long = "checksum-file", alias = "checksums")]
    pub checksum_file: Option<PathBuf>,

    /// Hash algorithm (auto-detected from checksum file if not specified)
    #[arg(long = "checksum-type", value_parser = parse_algorithm)]
    pub checksum_type: Option<HashAlgorithm>,

    /// Require filename match in addition to hash match
    #[arg(long)]
    pub match_names: bool,

    /// Filter which remote files to consider when matching
    #[arg(long)]
    pub glob: Option<String>,

    /// Filter remote files by IA format field (repeatable)
    #[arg(long)]
    pub format: Vec<String>,

    /// Filter remote files by source (original, derivative, metadata)
    #[arg(long)]
    pub source: Option<FileSource>,

    /// Batch verify from spreadsheet (CSV/TSV/XLSX/ODS/JSONL)
    #[arg(long, conflicts_with_all = ["identifier", "files"])]
    pub spreadsheet: Option<PathBuf>,

    /// Machine-readable JSONL output
    #[arg(long)]
    pub json: bool,
}

fn parse_algorithm(s: &str) -> std::result::Result<HashAlgorithm, String> {
    match s.to_lowercase().as_str() {
        "md5" => Ok(HashAlgorithm::Md5),
        "sha1" => Ok(HashAlgorithm::Sha1),
        "crc32" => Ok(HashAlgorithm::Crc32),
        _ => Err(format!(
            "unknown algorithm '{}' (valid: md5, sha1, crc32)",
            s
        )),
    }
}

pub async fn run(client: &IaClient, args: VerifyArgs, quiet: u8, jobs: usize) -> Result<()> {
    // Placeholder — will be implemented in Task 6
    let _ = (client, args, quiet, jobs);
    todo!("verify command handler")
}
```

- [ ] **Step 2: Register the command in mod.rs**

In `ia-cli/src/commands/mod.rs`, add in alphabetical order:

```rust
pub mod verify;
```

- [ ] **Step 3: Register in main.rs — add to Commands enum**

In `ia-cli/src/main.rs`, add to the `Commands` enum (after `Upload`):

```rust
    /// Verify local files exist on archive.org with matching checksums
    #[command(visible_alias = "ve")]
    Verify(commands::verify::VerifyArgs),
```

- [ ] **Step 4: Add dispatch in main.rs match block**

In the `match cli.command` block in `main()`, add before `Commands::Completions`:

```rust
        Commands::Verify(args) => {
            commands::verify::run(&client, args, cli.quiet, cli.jobs).await?
        }
```

- [ ] **Step 5: Verify it compiles**

Run: `cargo check -p ia-cli`
Expected: success (handler is `todo!()` but compiles)

- [ ] **Step 6: Commit**

```bash
git add ia-cli/src/commands/verify.rs ia-cli/src/commands/mod.rs ia-cli/src/main.rs
git commit -m "feat(verify): register ia verify CLI command with clap argument parsing

Wire up VerifyArgs struct with all flags from the design spec:
--checksum-file, --checksum-type, --match-names, --glob, --format,
--source, --spreadsheet, --json. Command alias: ve.
Handler is a placeholder todo!() — implementation next."
```

---

## Task 6: CLI Command — Single-Item Handler and Output

**Files:**
- Modify: `ia-cli/src/commands/verify.rs`

- [ ] **Step 1: Write CLI integration tests**

Create `ia-cli/tests/verify.rs`:

```rust
use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::io::Write;
use tempfile::NamedTempFile;

fn ia_with_config(config: &NamedTempFile) -> Command {
    let mut cmd = assert_cmd::cargo_bin_cmd!("ia");
    cmd.arg("--config-file")
        .arg(config.path())
        .env_remove("IA_S3_ACCESS")
        .env_remove("IA_S3_SECRET");
    cmd
}

fn empty_config() -> NamedTempFile {
    let f = NamedTempFile::new().unwrap();
    fs::write(f.path(), "").unwrap();
    f
}

// ─── Argument validation ─────────────────────────────────────────────────────

#[test]
fn verify_no_args_errors() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .arg("verify")
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

#[test]
fn verify_identifier_only_errors() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args(["verify", "my-item"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

#[test]
fn verify_alias_ve_works() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args(["ve", "my-item"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

#[test]
fn verify_invalid_checksum_type_errors() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args(["verify", "my-item", "file.txt", "--checksum-type", "sha512"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown algorithm"));
}

#[test]
fn verify_nonexistent_file_errors() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args(["verify", "my-item", "/tmp/ia-test-nonexistent-file-12345.txt"])
        .assert()
        .failure();
}

#[test]
fn verify_spreadsheet_conflicts_with_identifier() {
    let cfg = empty_config();
    let mut spreadsheet = NamedTempFile::new().unwrap();
    writeln!(spreadsheet, "identifier,file").unwrap();
    ia_with_config(&cfg)
        .args([
            "verify",
            "my-item",
            "--spreadsheet",
            spreadsheet.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn verify_checksum_file_satisfies_file_requirement() {
    // --checksum-file should satisfy the "files required" constraint
    // (will fail for other reasons like missing credentials, but NOT for missing files arg)
    let cfg = empty_config();
    let mut checksum_file = NamedTempFile::new().unwrap();
    writeln!(checksum_file, "d41d8cd98f00b204e9800998ecf8427e  file.txt").unwrap();
    ia_with_config(&cfg)
        .args([
            "verify",
            "my-item",
            "--checksum-file",
            checksum_file.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        // Should fail because of credentials, not because of missing files
        .stderr(predicate::str::contains("credentials"));
}
```

- [ ] **Step 2: Run tests to verify argument validation tests pass (and checksum_file test fails with credentials, not missing args)**

Run: `cargo test -p ia-cli --test verify`
Expected: most pass; the `todo!()` handler causes failures for tests that reach it

- [ ] **Step 3: Implement the single-item verify handler**

Replace the `run()` function in `ia-cli/src/commands/verify.rs`:

```rust
use console::style;
use std::collections::HashMap;

use ia_core::upload::checksum::{parse_checksums_multi, compute_file_hash_async};
use ia_core::verify::{
    verify_item, VerifyInput, VerifyOpts, VerifyResult, VerifyStatus,
};

pub async fn run(client: &IaClient, args: VerifyArgs, quiet: u8, jobs: usize) -> Result<()> {
    if let Some(spreadsheet_path) = &args.spreadsheet {
        return run_spreadsheet(client, &args, spreadsheet_path, quiet, jobs).await;
    }

    let identifier = args
        .identifier
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("identifier is required"))?;

    let (inputs, algorithm) = build_inputs(&args)?;

    let opts = VerifyOpts {
        algorithm,
        checksums: None,
        match_names: args.match_names,
        glob: args.glob.clone(),
        formats: args.format.clone(),
        source: args.source.clone(),
    };

    let results = verify_item(client, identifier, &inputs, &opts).await?;
    let has_failures = results.iter().any(|r| r.status != VerifyStatus::Verified);

    print_results(identifier, &results, &args, quiet);

    if has_failures {
        std::process::exit(1);
    }

    Ok(())
}

fn build_inputs(args: &VerifyArgs) -> Result<(Vec<VerifyInput>, HashAlgorithm)> {
    let mut inputs = Vec::new();
    // None = auto-detect, Some = user explicitly set
    let explicit_algorithm = args.checksum_type.clone();
    let mut algorithm = explicit_algorithm.clone().unwrap_or(HashAlgorithm::Md5);

    // Load checksum file if provided
    if let Some(path) = &args.checksum_file {
        let content = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("cannot read checksum file: {e}"))?;

        let checksums = parse_checksums_multi(&content, explicit_algorithm.clone());

        // Auto-detect algorithm from first entry if user didn't explicitly set
        if explicit_algorithm.is_none() {
            if let Some(entry) = checksums.values().next() {
                algorithm = entry.algorithm.clone();
            }
        }

        for (filename, entry) in &checksums {
            inputs.push(VerifyInput::Hash {
                filename: filename.clone(),
                hash: entry.hash.clone(),
            });
        }
    }

    // Expand file/directory arguments
    for path in &args.files {
        if path.is_dir() {
            let entries = std::fs::read_dir(path)
                .map_err(|e| anyhow::anyhow!("cannot read directory {}: {e}", path.display()))?;
            for entry in entries {
                let entry = entry?;
                let entry_path = entry.path();
                if entry_path.is_file() {
                    inputs.push(VerifyInput::LocalFile(entry_path));
                }
            }
        } else if path.exists() {
            inputs.push(VerifyInput::LocalFile(path.clone()));
        } else {
            anyhow::bail!("file not found: {}", path.display());
        }
    }

    if inputs.is_empty() {
        anyhow::bail!(
            "no files to verify — provide files, directories, or --checksum-file"
        );
    }

    Ok((inputs, algorithm))
}

fn print_results(identifier: &str, results: &[VerifyResult], args: &VerifyArgs, quiet: u8) {
    if args.json {
        print_json_results(results);
        return;
    }

    if quiet >= 1 {
        return;
    }

    let verified = results.iter().filter(|r| r.status == VerifyStatus::Verified).count();
    let missing = results.iter().filter(|r| r.status == VerifyStatus::Missing).count();
    let mismatched = results.iter().filter(|r| r.status == VerifyStatus::Mismatch).count();
    let errors = results.iter().filter(|r| r.status == VerifyStatus::Error).count();
    let failures = missing + mismatched + errors;

    eprintln!(
        "{} {}  ({} files)",
        style("▸").cyan(),
        style(identifier).bold(),
        results.len()
    );

    for r in results {
        match r.status {
            VerifyStatus::Verified => {
                let size_str = r
                    .bytes
                    .map(|b| format_size(b))
                    .unwrap_or_default();
                let hash_short = r
                    .local_hash
                    .as_deref()
                    .map(|h| if h.len() > 8 { &h[..8] } else { h })
                    .unwrap_or("");
                let remote_note = if r.remote_key.as_deref() != Some(&r.local_file) {
                    r.remote_key
                        .as_deref()
                        .map(|k| format!("  (remote: {})", k))
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                eprintln!(
                    "  {} {:<20} {:>10}  {}…{}",
                    style("✓").green(),
                    r.local_file,
                    size_str,
                    hash_short,
                    style(remote_note).dim(),
                );
            }
            VerifyStatus::Missing => {
                eprintln!(
                    "  {} {:<20} {}",
                    style("✗").red(),
                    r.local_file,
                    style("no matching hash on remote").dim(),
                );
            }
            VerifyStatus::Mismatch => {
                let local_h = r.local_hash.as_deref().unwrap_or("?");
                let remote_h = r.remote_hash.as_deref().unwrap_or("?");
                eprintln!(
                    "  {} {:<20} {} mismatch (local: {}… remote: {}…)",
                    style("✗").red(),
                    r.local_file,
                    r.algorithm,
                    &local_h[..local_h.len().min(8)],
                    &remote_h[..remote_h.len().min(8)],
                );
            }
            VerifyStatus::Error => {
                let detail = r.detail.as_deref().unwrap_or("unknown error");
                eprintln!(
                    "  {} {:<20} {}",
                    style("✗").red(),
                    r.local_file,
                    style(detail).dim(),
                );
            }
        }
    }

    // Summary line
    eprintln!();
    if failures > 0 {
        eprint!(
            "{}  {} {}",
            identifier,
            failures,
            if failures == 1 { "error" } else { "errors" }
        );
    } else {
        eprint!("{}", identifier);
    }
    eprintln!();

    let mut parts = Vec::new();
    if verified > 0 {
        parts.push(format!("{} {} verified", style("✓").green(), verified));
    }
    if missing > 0 {
        parts.push(format!("{} {} missing", style("✗").red(), missing));
    }
    if mismatched > 0 {
        parts.push(format!("{} {} mismatch", style("✗").red(), mismatched));
    }
    if errors > 0 {
        parts.push(format!("{} {} errors", style("✗").red(), errors));
    }
    eprintln!("  {}", parts.join(" · "));
}

fn print_json_results(results: &[VerifyResult]) {
    for r in results {
        match r.status {
            VerifyStatus::Error => {
                // Error results go to stderr
                if let Ok(json) = serde_json::to_string(r) {
                    eprintln!("{json}");
                }
            }
            _ => {
                if let Ok(json) = serde_json::to_string(r) {
                    println!("{json}");
                }
            }
        }
    }
}

fn format_size(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

    let b = bytes as f64;
    if b < KIB {
        format!("{bytes} B")
    } else if b < MIB {
        format!("{:.0} KiB", b / KIB)
    } else if b < GIB {
        format!("{:.1} MiB", b / MIB)
    } else {
        format!("{:.2} GiB", b / GIB)
    }
}

async fn run_spreadsheet(
    _client: &IaClient,
    _args: &VerifyArgs,
    _spreadsheet_path: &std::path::Path,
    _quiet: u8,
    _jobs: usize,
) -> Result<()> {
    // Placeholder — will be implemented in Task 7
    todo!("spreadsheet verify handler")
}
```

- [ ] **Step 4: Run CLI integration tests**

Run: `cargo test -p ia-cli --test verify`
Expected: argument validation tests pass

- [ ] **Step 5: Commit**

```bash
git add ia-cli/src/commands/verify.rs ia-cli/tests/verify.rs
git commit -m "feat(verify): implement single-item verify handler with console/JSON output

Handle file/directory inputs, --checksum-file, and --checksum-type.
Console output shows per-file status with icons and summary.
JSON mode outputs JSONL (errors to stderr).
Exit code 1 if any file not verified.
Spreadsheet handler is a placeholder."
```

---

## Task 7: Spreadsheet Verify Handler

**Files:**
- Modify: `ia-cli/src/commands/verify.rs`

- [ ] **Step 1: Write spreadsheet integration tests**

Add to `ia-cli/tests/verify.rs`:

```rust
#[test]
fn verify_spreadsheet_missing_identifier_column_errors() {
    let cfg = empty_config();
    let mut spreadsheet = NamedTempFile::with_suffix(".csv").unwrap();
    writeln!(spreadsheet, "file,md5").unwrap();
    writeln!(spreadsheet, "test.txt,abc123").unwrap();
    ia_with_config(&cfg)
        .args([
            "verify",
            "--spreadsheet",
            spreadsheet.path().to_str().unwrap(),
        ])
        .assert()
        .failure();
}

#[test]
fn verify_spreadsheet_missing_file_column_errors() {
    let cfg = empty_config();
    let mut spreadsheet = NamedTempFile::with_suffix(".csv").unwrap();
    writeln!(spreadsheet, "identifier,md5").unwrap();
    writeln!(spreadsheet, "test-item,abc123").unwrap();
    ia_with_config(&cfg)
        .args([
            "verify",
            "--spreadsheet",
            spreadsheet.path().to_str().unwrap(),
        ])
        .assert()
        .failure();
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-cli --test verify -- spreadsheet`
Expected: tests hit the `todo!()` panic

- [ ] **Step 3: Implement run_spreadsheet()**

Replace the `run_spreadsheet` placeholder in `ia-cli/src/commands/verify.rs`:

```rust
async fn run_spreadsheet(
    client: &IaClient,
    args: &VerifyArgs,
    spreadsheet_path: &std::path::Path,
    quiet: u8,
    jobs: usize,
) -> Result<()> {
    use ia_core::spreadsheet::read_spreadsheet;
    use ia_core::verify::{verify_item, VerifyInput, VerifyOpts, VerifyStatus};

    let records = read_spreadsheet(spreadsheet_path)?;
    if records.is_empty() {
        anyhow::bail!("spreadsheet is empty");
    }

    // Validate required columns from first record
    let first = &records[0];
    if first.1.get("file").is_none() {
        anyhow::bail!(
            "spreadsheet must have a 'file' column"
        );
    }

    // Determine which hash column to use
    let algorithm = args.checksum_type.clone().unwrap_or(HashAlgorithm::Md5);
    let hash_col = algorithm.field_name();

    // Group records by identifier, deduplicate identical identifier+file pairs
    use std::collections::HashSet;
    let mut by_identifier: Vec<(String, Vec<VerifyInput>)> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();

    for (identifier, fields) in &records {
        let file_val = fields
            .get("file")
            .ok_or_else(|| anyhow::anyhow!("row missing 'file' column"))?;

        // Deduplicate identical identifier+file rows
        if !seen.insert((identifier.clone(), file_val.clone())) {
            continue;
        }

        let input = if let Some(hash) = fields.get(hash_col).filter(|h| !h.is_empty()) {
            // Use pre-computed hash from spreadsheet column
            VerifyInput::Hash {
                filename: file_val.clone(),
                hash: hash.clone(),
            }
        } else {
            // Need local file
            let path = PathBuf::from(file_val);
            if !path.exists() {
                anyhow::bail!(
                    "file not found: {} — provide --checksum-file or add {} column to spreadsheet",
                    path.display(),
                    hash_col
                );
            }
            VerifyInput::LocalFile(path)
        };

        if let Some(group) = by_identifier.iter_mut().find(|(id, _)| id == identifier) {
            group.1.push(input);
        } else {
            by_identifier.push((identifier.clone(), vec![input]));
        }
    }

    // Verify each identifier
    let opts = VerifyOpts {
        algorithm,
        checksums: None,
        match_names: args.match_names,
        glob: args.glob.clone(),
        formats: args.format.clone(),
        source: args.source.clone(),
    };

    let mut all_results: Vec<(String, Vec<VerifyResult>)> = Vec::new();
    let mut has_failures = false;

    // Process items with concurrency, preserving spreadsheet order
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(jobs));
    let mut handles = Vec::new();

    for (identifier, inputs) in by_identifier {
        let client = client.clone();
        let opts = opts.clone();
        let sem = semaphore.clone();

        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let results = verify_item(&client, &identifier, &inputs, &opts).await?;
            Ok::<_, anyhow::Error>((identifier, results))
        }));
    }

    // Collect in spawn order (preserves spreadsheet identifier order)
    for handle in handles {
        let (identifier, results) = handle.await??;
        if results.iter().any(|r| r.status != VerifyStatus::Verified) {
            has_failures = true;
        }
        all_results.push((identifier, results));
    }

    // Print output
    print_batch_results(&all_results, args, quiet);

    if has_failures {
        std::process::exit(1);
    }

    Ok(())
}

fn print_batch_results(
    all_results: &[(String, Vec<VerifyResult>)],
    args: &VerifyArgs,
    quiet: u8,
) {
    if args.json {
        for (_, results) in all_results {
            print_json_results(results);
        }
        return;
    }

    if quiet >= 1 {
        return;
    }

    let mut total_verified = 0usize;
    let mut total_failures = 0usize;
    let mut total_files = 0usize;

    for (identifier, results) in all_results {
        print_results(identifier, results, args, quiet);
        total_files += results.len();
        total_verified += results.iter().filter(|r| r.status == VerifyStatus::Verified).count();
        total_failures += results
            .iter()
            .filter(|r| r.status != VerifyStatus::Verified)
            .count();
        eprintln!();
    }

    // Batch summary
    eprintln!(
        "Verified {} items, {} files",
        all_results.len(),
        total_files
    );
    let mut parts = Vec::new();
    if total_verified > 0 {
        parts.push(format!("{} {} verified", style("✓").green(), total_verified));
    }
    if total_failures > 0 {
        parts.push(format!("{} {} failed", style("✗").red(), total_failures));
    }
    eprintln!("  {}", parts.join(" · "));
}
```

Note: This requires `IaClient` to implement `Clone`. Check if it does — if not, wrap it in an `Arc` at the call site. The `IaClient` in this project wraps `reqwest_middleware::ClientWithMiddleware` which is already `Clone`.

- [ ] **Step 4: Run spreadsheet tests**

Run: `cargo test -p ia-cli --test verify -- spreadsheet`
Expected: tests pass

- [ ] **Step 5: Commit**

```bash
git add ia-cli/src/commands/verify.rs ia-cli/tests/verify.rs
git commit -m "feat(verify): add spreadsheet batch verification handler

Read identifier + file columns from spreadsheet, group by identifier,
verify concurrently with -j semaphore. Supports optional hash columns
(md5/sha1/crc32) from spreadsheet to skip local hashing.
Batch summary output shows aggregate counts."
```

---

## Task 8: Wiremock CLI Integration Tests

**Files:**
- Modify: `ia-cli/tests/verify.rs`

These tests use wiremock to mock the metadata API and test the full verify flow end-to-end.

- [ ] **Step 1: Add wiremock integration tests**

Add to `ia-cli/tests/verify.rs`:

```rust
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ia_cmd() -> Command {
    assert_cmd::cargo_bin_cmd!("ia")
}

#[tokio::test]
async fn verify_all_files_verified_exit_0() {
    let server = MockServer::start().await;

    // Create temp file with known content
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("hello.txt");
    fs::write(&file_path, b"hello").unwrap();

    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "test-item"},
            "files": [{
                "name": "hello.txt",
                "source": "original",
                "md5": "5d41402abc4b2a76b9719d911017c592",
                "sha1": "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d",
                "crc32": "3610a686",
                "size": "5"
            }]
        })))
        .mount(&server)
        .await;

    let host = server.uri().replace("http://", "");
    ia_cmd()
        .args(["--insecure", "-H", &host, "verify", "test-item"])
        .arg(file_path.to_str().unwrap())
        .env("IA_S3_ACCESS", "test")
        .env("IA_S3_SECRET", "test")
        .assert()
        .success();
}

#[tokio::test]
async fn verify_missing_file_exit_1() {
    let server = MockServer::start().await;

    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("hello.txt");
    fs::write(&file_path, b"hello").unwrap();

    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "test-item"},
            "files": [{
                "name": "other.txt",
                "md5": "different_hash",
                "size": "100"
            }]
        })))
        .mount(&server)
        .await;

    let host = server.uri().replace("http://", "");
    ia_cmd()
        .args(["--insecure", "-H", &host, "verify", "test-item"])
        .arg(file_path.to_str().unwrap())
        .env("IA_S3_ACCESS", "test")
        .env("IA_S3_SECRET", "test")
        .assert()
        .failure();
}

#[tokio::test]
async fn verify_json_output_format() {
    let server = MockServer::start().await;

    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("hello.txt");
    fs::write(&file_path, b"hello").unwrap();

    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "test-item"},
            "files": [{
                "name": "hello.txt",
                "md5": "5d41402abc4b2a76b9719d911017c592",
                "size": "5"
            }]
        })))
        .mount(&server)
        .await;

    let host = server.uri().replace("http://", "");
    let output = ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "verify",
            "test-item",
            "--json",
        ])
        .arg(file_path.to_str().unwrap())
        .env("IA_S3_ACCESS", "test")
        .env("IA_S3_SECRET", "test")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"status\":\"verified\""));
    assert!(stdout.contains("\"identifier\":\"test-item\""));
    assert!(stdout.contains("\"local_file\":\"hello.txt\""));
}

#[tokio::test]
async fn verify_quiet_no_output() {
    let server = MockServer::start().await;

    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("hello.txt");
    fs::write(&file_path, b"hello").unwrap();

    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "test-item"},
            "files": [{
                "name": "hello.txt",
                "md5": "5d41402abc4b2a76b9719d911017c592",
                "size": "5"
            }]
        })))
        .mount(&server)
        .await;

    let host = server.uri().replace("http://", "");
    let output = ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "-q",
            "verify",
            "test-item",
        ])
        .arg(file_path.to_str().unwrap())
        .env("IA_S3_ACCESS", "test")
        .env("IA_S3_SECRET", "test")
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
}

#[tokio::test]
async fn verify_checksum_file_no_local_files() {
    let server = MockServer::start().await;

    let mut checksum_file = NamedTempFile::new().unwrap();
    // MD5 of "hello" = 5d41402abc4b2a76b9719d911017c592
    writeln!(
        checksum_file,
        "5d41402abc4b2a76b9719d911017c592  hello.txt"
    )
    .unwrap();

    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "test-item"},
            "files": [{
                "name": "hello.txt",
                "md5": "5d41402abc4b2a76b9719d911017c592",
                "size": "5"
            }]
        })))
        .mount(&server)
        .await;

    let host = server.uri().replace("http://", "");
    ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "verify",
            "test-item",
            "--checksum-file",
            checksum_file.path().to_str().unwrap(),
        ])
        .env("IA_S3_ACCESS", "test")
        .env("IA_S3_SECRET", "test")
        .assert()
        .success();
}

#[tokio::test]
async fn verify_sha1_checksum_type() {
    let server = MockServer::start().await;

    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("hello.txt");
    fs::write(&file_path, b"hello").unwrap();

    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "test-item"},
            "files": [{
                "name": "hello.txt",
                "md5": "5d41402abc4b2a76b9719d911017c592",
                "sha1": "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d",
                "size": "5"
            }]
        })))
        .mount(&server)
        .await;

    let host = server.uri().replace("http://", "");
    ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "verify",
            "test-item",
            "--checksum-type",
            "sha1",
        ])
        .arg(file_path.to_str().unwrap())
        .env("IA_S3_ACCESS", "test")
        .env("IA_S3_SECRET", "test")
        .assert()
        .success();
}

#[tokio::test]
async fn verify_match_names_mismatch_exit_1() {
    let server = MockServer::start().await;

    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("hello.txt");
    fs::write(&file_path, b"hello").unwrap();

    // Remote has same hash but different name — --match-names should fail
    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "test-item"},
            "files": [{
                "name": "renamed.txt",
                "md5": "5d41402abc4b2a76b9719d911017c592",
                "size": "5"
            }]
        })))
        .mount(&server)
        .await;

    let host = server.uri().replace("http://", "");
    // Without --match-names: should succeed (hash matches)
    ia_cmd()
        .args(["--insecure", "-H", &host, "verify", "test-item"])
        .arg(file_path.to_str().unwrap())
        .env("IA_S3_ACCESS", "test")
        .env("IA_S3_SECRET", "test")
        .assert()
        .success();

    // With --match-names: should fail (name doesn't match)
    ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "verify",
            "test-item",
            "--match-names",
        ])
        .arg(file_path.to_str().unwrap())
        .env("IA_S3_ACCESS", "test")
        .env("IA_S3_SECRET", "test")
        .assert()
        .failure();
}

#[tokio::test]
async fn verify_glob_filters_remote_files() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "test-item"},
            "files": [
                {
                    "name": "file.txt",
                    "md5": "abc123",
                    "size": "100"
                },
                {
                    "name": "file.pdf",
                    "md5": "abc123",
                    "size": "100"
                }
            ]
        })))
        .mount(&server)
        .await;

    let mut checksum_file = NamedTempFile::new().unwrap();
    writeln!(checksum_file, "abc123def456abc123def456abc123de  local.txt").unwrap();

    let host = server.uri().replace("http://", "");

    // Without --glob: should find hash match in either file
    // With --glob '*.pdf': should only match against file.pdf
    // The hash "abc123def456abc123def456abc123de" won't match "abc123"
    // so this tests that glob filtering is applied

    // Let's use a hash that DOES match, and verify glob picks the right file
    let mut checksum_file2 = NamedTempFile::new().unwrap();
    writeln!(checksum_file2, "abc123  local.txt").unwrap();

    ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "verify",
            "test-item",
            "--checksum-file",
            checksum_file2.path().to_str().unwrap(),
            "--glob",
            "*.pdf",
            "--json",
        ])
        .env("IA_S3_ACCESS", "test")
        .env("IA_S3_SECRET", "test")
        .output()
        .map(|o| {
            let stdout = String::from_utf8_lossy(&o.stdout);
            // Should match file.pdf (passes glob filter)
            assert!(stdout.contains("\"remote_key\":\"file.pdf\""));
        })
        .unwrap();
}
```

- [ ] **Step 2: Add network error and source filter tests**

Add to `ia-cli/tests/verify.rs`:

```rust
#[tokio::test]
async fn verify_metadata_fetch_error_exit_1() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(500).set_body_string("Internal Server Error"))
        .mount(&server)
        .await;

    let mut checksum_file = NamedTempFile::new().unwrap();
    writeln!(checksum_file, "abc123def456abc123def456abc123de  file.txt").unwrap();

    let host = server.uri().replace("http://", "");
    ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "verify",
            "test-item",
            "--checksum-file",
            checksum_file.path().to_str().unwrap(),
        ])
        .env("IA_S3_ACCESS", "test")
        .env("IA_S3_SECRET", "test")
        .assert()
        .failure();
}

#[tokio::test]
async fn verify_source_filter_original_only() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "test-item"},
            "files": [
                {
                    "name": "file.txt",
                    "source": "original",
                    "md5": "abc123",
                    "size": "100"
                },
                {
                    "name": "file_thumb.txt",
                    "source": "derivative",
                    "md5": "abc123",
                    "size": "50"
                }
            ]
        })))
        .mount(&server)
        .await;

    let mut checksum_file = NamedTempFile::new().unwrap();
    writeln!(checksum_file, "abc123  local.txt").unwrap();

    let host = server.uri().replace("http://", "");
    let output = ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "verify",
            "test-item",
            "--checksum-file",
            checksum_file.path().to_str().unwrap(),
            "--source",
            "original",
            "--json",
        ])
        .env("IA_S3_ACCESS", "test")
        .env("IA_S3_SECRET", "test")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Should match file.txt (original), not file_thumb.txt (derivative)
    assert!(stdout.contains("\"remote_key\":\"file.txt\""));
}

#[tokio::test]
async fn verify_spreadsheet_with_hash_column_e2e() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "test-item"},
            "files": [{
                "name": "file.txt",
                "md5": "abc123",
                "size": "100"
            }]
        })))
        .mount(&server)
        .await;

    let mut spreadsheet = NamedTempFile::with_suffix(".csv").unwrap();
    writeln!(spreadsheet, "identifier,file,md5").unwrap();
    writeln!(spreadsheet, "test-item,file.txt,abc123").unwrap();

    let host = server.uri().replace("http://", "");
    ia_cmd()
        .args([
            "--insecure",
            "-H",
            &host,
            "verify",
            "--spreadsheet",
            spreadsheet.path().to_str().unwrap(),
            "--json",
        ])
        .env("IA_S3_ACCESS", "test")
        .env("IA_S3_SECRET", "test")
        .assert()
        .success();
}
```

- [ ] **Step 3: Run all tests**

Run: `cargo test -p ia-cli --test verify`
Expected: all tests pass

- [ ] **Step 4: Commit**

```bash
git add ia-cli/tests/verify.rs
git commit -m "test(verify): add wiremock CLI integration tests

Full end-to-end tests: exit codes, JSON output format, quiet mode,
checksum-file without local files, SHA-1 checksum type, --match-names
behavior, --glob filtering, --source filtering, network error handling,
spreadsheet with hash column. 17+ total CLI tests."
```

---

## Task 9: Run Full CI Suite

**Files:** None (verification only)

- [ ] **Step 1: Run `just ci`**

Run: `just ci`
Expected: fmt-check, check, test, doc all pass

- [ ] **Step 2: Fix any issues**

If any test or lint fails, fix and re-run.

- [ ] **Step 3: Final commit if fixes needed**

```bash
git add -A
git commit -m "fix: address CI issues in verify implementation"
```

---

## Task 10: Push Branch and Create PR

**Files:** None

- [ ] **Step 1: Push branch**

```bash
git push -u origin HEAD
```

- [ ] **Step 2: Create PR**

```bash
gh pr create --title "feat: add ia verify command" --body "$(cat <<'EOF'
## Summary

Add `ia verify` — a read-only command that asserts local files exist on
archive.org with matching checksums. Exits non-zero if any file can't be
verified.

- Hash-only matching by default (content integrity over filenames)
- `--match-names` for strict filename+hash verification
- Multi-algorithm: `--checksum-type md5|sha1|crc32` with auto-detection
- `--checksum-file` for pre-computed hashes (no local files needed)
- `--spreadsheet` for batch verification (same format as `ia upload import`)
- Standard output modes: console, `--json` (JSONL), `-q` (exit code only)

Closes #TBD

## Test plan

- [ ] Unit tests: hash computation (MD5/SHA-1/CRC32), checksum parsing, verify matching logic
- [ ] Integration tests: wiremock metadata mocking, all verification modes
- [ ] CLI tests: argument validation, exit codes, output formats, glob/format/source filtering

EOF
)"
```
