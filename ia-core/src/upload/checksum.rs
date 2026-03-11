use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use md5::{Digest, Md5};

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
}
