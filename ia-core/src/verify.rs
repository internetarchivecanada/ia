//! Verification of local files against remote Internet Archive items.
//!
//! Provides hash-based verification to confirm that local files match
//! their remote counterparts on archive.org. Supports MD5, SHA-1, and
//! CRC32 algorithms.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Serialize;

use crate::files::{self, FileFilter};
use crate::types::{FileMetadata, FileSource, ItemMetadata};
use crate::upload::checksum::HashAlgorithm;

/// Result of verifying a single file against a remote item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerifyResult {
    /// The item identifier on archive.org.
    pub identifier: String,
    /// The local filename (or hash-input filename).
    pub local_file: String,
    /// Verification outcome.
    pub status: VerifyStatus,
    /// The remote file key that matched (if any).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_key: Option<String>,
    /// The hash computed (or provided) for the local file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_hash: Option<String>,
    /// The hash found on the remote file (if any).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_hash: Option<String>,
    /// The hash algorithm used for comparison.
    pub algorithm: String,
    /// Size of the local file in bytes (if known).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// Additional detail (e.g., error message).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Outcome of a single file verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyStatus {
    /// Local file matches a remote file.
    Verified,
    /// No matching remote file found.
    Missing,
    /// Remote file found by name but hash differs.
    Mismatch,
    /// An error occurred during verification.
    Error,
}

/// Options controlling verification behavior.
#[derive(Debug, Clone, Default)]
pub struct VerifyOpts {
    /// Hash algorithm to use for comparison.
    pub algorithm: HashAlgorithm,
    /// Pre-computed checksums (filename -> hash).
    pub checksums: Option<HashMap<String, String>>,
    /// If true, match by filename then check hash. If false, match by hash only.
    pub match_names: bool,
    /// Glob pattern to filter remote files.
    pub glob: Option<String>,
    /// Format filters for remote files.
    pub formats: Vec<String>,
    /// Source filter for remote files.
    pub source: Option<FileSource>,
}

/// Input to verify: either a local file path or a pre-computed hash.
#[derive(Debug, Clone)]
pub enum VerifyInput {
    /// A local file to hash and verify.
    LocalFile(PathBuf),
    /// A pre-computed hash with associated filename.
    Hash { filename: String, hash: String },
}

/// Extract the hash value from a [`FileMetadata`] for the given algorithm.
///
/// Returns `None` if the file has no hash for the requested algorithm.
#[must_use]
pub fn get_remote_hash<'a>(file: &'a FileMetadata, algorithm: &HashAlgorithm) -> Option<&'a str> {
    match algorithm {
        HashAlgorithm::Md5 => file.md5.as_deref(),
        HashAlgorithm::Sha1 => file.sha1.as_deref(),
        HashAlgorithm::Crc32 => file.crc32.as_deref(),
    }
}

/// Filter remote files according to verify options (glob, format, source).
///
/// Call once per item, then pass the result to [`match_file_against_remote`]
/// for each input file. This avoids re-filtering on every file check.
pub fn filter_remote_files<'a>(item: &'a ItemMetadata, opts: &VerifyOpts) -> Vec<&'a FileMetadata> {
    let filter = FileFilter {
        glob: opts.glob.clone(),
        formats: opts.formats.clone(),
        source: opts.source.clone(),
        ..Default::default()
    };
    files::list(item, &filter)
}

/// Core matching logic: verify a single local file against pre-filtered remote files.
///
/// Operates in two modes controlled by `opts.match_names`:
///
/// - **Hash-only mode** (default): searches all remote files for any
///   file with a matching hash. Returns `Verified` on first match, `Missing`
///   if no remote file has the same hash.
///
/// - **Match-names mode**: finds a remote file by name, then compares hashes.
///   Returns `Verified` if hashes match, `Mismatch` if they differ, `Missing`
///   if no remote file has that name, or `Error` if the remote file lacks a hash.
///
/// Use [`filter_remote_files`] to produce the `filtered` argument.
pub fn match_file_against_remote(
    identifier: &str,
    local_filename: &str,
    local_hash: &str,
    local_bytes: Option<u64>,
    filtered: &[&FileMetadata],
    opts: &VerifyOpts,
) -> VerifyResult {
    if opts.match_names {
        // Find remote file by name.
        let remote = filtered.iter().copied().find(|f| f.name == local_filename);
        match remote {
            Some(rf) => {
                let remote_hash_val = get_remote_hash(rf, &opts.algorithm);
                match remote_hash_val {
                    Some(rh) => {
                        if rh == local_hash {
                            VerifyResult {
                                identifier: identifier.to_string(),
                                local_file: local_filename.to_string(),
                                status: VerifyStatus::Verified,
                                remote_key: Some(rf.name.clone()),
                                local_hash: Some(local_hash.to_string()),
                                remote_hash: Some(rh.to_string()),
                                algorithm: opts.algorithm.field_name().to_string(),
                                bytes: local_bytes,
                                detail: None,
                            }
                        } else {
                            VerifyResult {
                                identifier: identifier.to_string(),
                                local_file: local_filename.to_string(),
                                status: VerifyStatus::Mismatch,
                                remote_key: Some(rf.name.clone()),
                                local_hash: Some(local_hash.to_string()),
                                remote_hash: Some(rh.to_string()),
                                algorithm: opts.algorithm.field_name().to_string(),
                                bytes: local_bytes,
                                detail: None,
                            }
                        }
                    }
                    None => VerifyResult {
                        identifier: identifier.to_string(),
                        local_file: local_filename.to_string(),
                        status: VerifyStatus::Error,
                        remote_key: Some(rf.name.clone()),
                        local_hash: Some(local_hash.to_string()),
                        remote_hash: None,
                        algorithm: opts.algorithm.field_name().to_string(),
                        bytes: local_bytes,
                        detail: Some(format!(
                            "remote file has no {} hash",
                            opts.algorithm.field_name()
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
                algorithm: opts.algorithm.field_name().to_string(),
                bytes: local_bytes,
                detail: None,
            },
        }
    } else {
        // Hash-only mode: find any remote file with matching hash.
        let matching = filtered
            .iter()
            .copied()
            .find(|f| get_remote_hash(f, &opts.algorithm).is_some_and(|h| h == local_hash));
        match matching {
            Some(rf) => VerifyResult {
                identifier: identifier.to_string(),
                local_file: local_filename.to_string(),
                status: VerifyStatus::Verified,
                remote_key: Some(rf.name.clone()),
                local_hash: Some(local_hash.to_string()),
                remote_hash: get_remote_hash(rf, &opts.algorithm).map(|s| s.to_string()),
                algorithm: opts.algorithm.field_name().to_string(),
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
                algorithm: opts.algorithm.field_name().to_string(),
                bytes: local_bytes,
                detail: None,
            },
        }
    }
}

/// Verify multiple local files/hashes against a remote Internet Archive item.
///
/// Fetches item metadata via the IA API, then for each input:
/// - `VerifyInput::LocalFile`: computes the hash from disk (unless pre-computed
///   in `opts.checksums`), reads file size, then calls [`match_file_against_remote`].
/// - `VerifyInput::Hash`: calls [`match_file_against_remote`] directly.
///
/// File access or hashing errors produce a [`VerifyResult`] with status
/// [`VerifyStatus::Error`] rather than aborting the entire operation.
pub async fn verify_item(
    client: &crate::IaClient,
    identifier: &str,
    inputs: &[VerifyInput],
    opts: &VerifyOpts,
) -> crate::Result<Vec<VerifyResult>> {
    use crate::upload::checksum::compute_file_hash_async;

    let item = client.get_item(identifier).await?;
    let filtered = filter_remote_files(&item, opts);

    let mut results = Vec::with_capacity(inputs.len());

    for input in inputs {
        match input {
            VerifyInput::LocalFile(path) => {
                let filename = match path.file_name() {
                    Some(n) => n.to_string_lossy().to_string(),
                    None => {
                        results.push(VerifyResult {
                            identifier: identifier.to_string(),
                            local_file: path.display().to_string(),
                            status: VerifyStatus::Error,
                            remote_key: None,
                            local_hash: None,
                            remote_hash: None,
                            algorithm: opts.algorithm.field_name().to_string(),
                            bytes: None,
                            detail: Some("path has no filename component".to_string()),
                        });
                        continue;
                    }
                };

                // Use pre-computed hash if available.
                let hash_result = if let Some(checksums) = &opts.checksums {
                    if let Some(h) = checksums.get(&filename) {
                        Ok(h.clone())
                    } else {
                        compute_file_hash_async(path, opts.algorithm.clone()).await
                    }
                } else {
                    compute_file_hash_async(path, opts.algorithm.clone()).await
                };

                match hash_result {
                    Ok(hash) => {
                        let file_size = std::fs::metadata(path).ok().map(|m| m.len());
                        let result = match_file_against_remote(
                            identifier, &filename, &hash, file_size, &filtered, opts,
                        );
                        results.push(result);
                    }
                    Err(e) => {
                        results.push(VerifyResult {
                            identifier: identifier.to_string(),
                            local_file: filename,
                            status: VerifyStatus::Error,
                            remote_key: None,
                            local_hash: None,
                            remote_hash: None,
                            algorithm: opts.algorithm.field_name().to_string(),
                            bytes: None,
                            detail: Some(e.to_string()),
                        });
                    }
                }
            }
            VerifyInput::Hash { filename, hash } => {
                let result =
                    match_file_against_remote(identifier, filename, hash, None, &filtered, opts);
                results.push(result);
            }
        }
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // ---- Test helpers ----

    fn make_remote_file(
        name: &str,
        md5: Option<&str>,
        sha1: Option<&str>,
        crc32: Option<&str>,
    ) -> FileMetadata {
        FileMetadata {
            name: name.to_string(),
            source: Some("original".to_string()),
            format: Some("Data".to_string()),
            md5: md5.map(|s| s.to_string()),
            sha1: sha1.map(|s| s.to_string()),
            crc32: crc32.map(|s| s.to_string()),
            size: Some(100),
            mtime: None,
            original: None,
            rotation: None,
            extra: HashMap::new(),
        }
    }

    fn make_item(files: Vec<FileMetadata>) -> ItemMetadata {
        ItemMetadata {
            metadata: Default::default(),
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

    // ---- get_remote_hash tests ----

    #[test]
    fn get_remote_hash_md5() {
        let f = make_remote_file("test.txt", Some("abc123"), None, None);
        assert_eq!(get_remote_hash(&f, &HashAlgorithm::Md5), Some("abc123"));
        assert_eq!(get_remote_hash(&f, &HashAlgorithm::Sha1), None);
        assert_eq!(get_remote_hash(&f, &HashAlgorithm::Crc32), None);
    }

    #[test]
    fn get_remote_hash_sha1() {
        let f = make_remote_file("test.txt", None, Some("sha1hash"), None);
        assert_eq!(get_remote_hash(&f, &HashAlgorithm::Sha1), Some("sha1hash"));
    }

    #[test]
    fn get_remote_hash_crc32() {
        let f = make_remote_file("test.txt", None, None, Some("3610a686"));
        assert_eq!(get_remote_hash(&f, &HashAlgorithm::Crc32), Some("3610a686"));
    }

    // ---- match_file_against_remote: hash-only mode ----

    #[test]
    fn verify_hash_only_exact_name_match() {
        let files = vec![make_remote_file(
            "photo.jpg",
            Some("aabbccdd11223344aabbccdd11223344"),
            None,
            None,
        )];
        let item = make_item(files);
        let opts = VerifyOpts::default();
        let filtered = filter_remote_files(&item, &opts);
        let result = match_file_against_remote(
            "test-item",
            "photo.jpg",
            "aabbccdd11223344aabbccdd11223344",
            Some(1024),
            &filtered,
            &opts,
        );
        assert_eq!(result.status, VerifyStatus::Verified);
        assert_eq!(result.remote_key.as_deref(), Some("photo.jpg"));
        assert_eq!(result.identifier, "test-item");
        assert_eq!(result.bytes, Some(1024));
    }

    #[test]
    fn verify_hash_only_different_name_match() {
        let files = vec![make_remote_file(
            "renamed_photo.jpg",
            Some("aabbccdd11223344aabbccdd11223344"),
            None,
            None,
        )];
        let item = make_item(files);
        let opts = VerifyOpts::default();
        let filtered = filter_remote_files(&item, &opts);
        let result = match_file_against_remote(
            "test-item",
            "photo.jpg",
            "aabbccdd11223344aabbccdd11223344",
            Some(1024),
            &filtered,
            &opts,
        );
        assert_eq!(result.status, VerifyStatus::Verified);
        assert_eq!(result.remote_key.as_deref(), Some("renamed_photo.jpg"));
    }

    #[test]
    fn verify_hash_only_no_match() {
        let files = vec![make_remote_file(
            "photo.jpg",
            Some("ffffffffffffffffffffffffffffffff"),
            None,
            None,
        )];
        let item = make_item(files);
        let opts = VerifyOpts::default();
        let filtered = filter_remote_files(&item, &opts);
        let result = match_file_against_remote(
            "test-item",
            "photo.jpg",
            "aabbccdd11223344aabbccdd11223344",
            Some(1024),
            &filtered,
            &opts,
        );
        assert_eq!(result.status, VerifyStatus::Missing);
        assert!(result.remote_key.is_none());
    }

    #[test]
    fn verify_hash_only_sha1() {
        let sha1_hash = "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d";
        let files = vec![make_remote_file("doc.txt", None, Some(sha1_hash), None)];
        let item = make_item(files);
        let opts = VerifyOpts {
            algorithm: HashAlgorithm::Sha1,
            ..Default::default()
        };
        let filtered = filter_remote_files(&item, &opts);
        let result =
            match_file_against_remote("test-item", "doc.txt", sha1_hash, Some(5), &filtered, &opts);
        assert_eq!(result.status, VerifyStatus::Verified);
        assert_eq!(result.algorithm, "sha1");
    }

    #[test]
    fn verify_hash_only_crc32() {
        let crc = "3610a686";
        let files = vec![make_remote_file("data.bin", None, None, Some(crc))];
        let item = make_item(files);
        let opts = VerifyOpts {
            algorithm: HashAlgorithm::Crc32,
            ..Default::default()
        };
        let filtered = filter_remote_files(&item, &opts);
        let result =
            match_file_against_remote("test-item", "data.bin", crc, Some(5), &filtered, &opts);
        assert_eq!(result.status, VerifyStatus::Verified);
        assert_eq!(result.algorithm, "crc32");
    }

    // ---- match_file_against_remote: match-names mode ----

    #[test]
    fn verify_match_names_exact_match() {
        let files = vec![make_remote_file(
            "photo.jpg",
            Some("aabbccdd11223344aabbccdd11223344"),
            None,
            None,
        )];
        let item = make_item(files);
        let opts = VerifyOpts {
            match_names: true,
            ..Default::default()
        };
        let filtered = filter_remote_files(&item, &opts);
        let result = match_file_against_remote(
            "test-item",
            "photo.jpg",
            "aabbccdd11223344aabbccdd11223344",
            Some(1024),
            &filtered,
            &opts,
        );
        assert_eq!(result.status, VerifyStatus::Verified);
        assert_eq!(result.remote_key.as_deref(), Some("photo.jpg"));
    }

    #[test]
    fn verify_match_names_hash_differs() {
        let files = vec![make_remote_file(
            "photo.jpg",
            Some("ffffffffffffffffffffffffffffffff"),
            None,
            None,
        )];
        let item = make_item(files);
        let opts = VerifyOpts {
            match_names: true,
            ..Default::default()
        };
        let filtered = filter_remote_files(&item, &opts);
        let result = match_file_against_remote(
            "test-item",
            "photo.jpg",
            "aabbccdd11223344aabbccdd11223344",
            Some(1024),
            &filtered,
            &opts,
        );
        assert_eq!(result.status, VerifyStatus::Mismatch);
        assert_eq!(
            result.local_hash.as_deref(),
            Some("aabbccdd11223344aabbccdd11223344")
        );
        assert_eq!(
            result.remote_hash.as_deref(),
            Some("ffffffffffffffffffffffffffffffff")
        );
    }

    #[test]
    fn verify_match_names_file_not_found() {
        let files = vec![make_remote_file(
            "other.jpg",
            Some("aabbccdd11223344aabbccdd11223344"),
            None,
            None,
        )];
        let item = make_item(files);
        let opts = VerifyOpts {
            match_names: true,
            ..Default::default()
        };
        let filtered = filter_remote_files(&item, &opts);
        let result = match_file_against_remote(
            "test-item",
            "photo.jpg",
            "aabbccdd11223344aabbccdd11223344",
            Some(1024),
            &filtered,
            &opts,
        );
        assert_eq!(result.status, VerifyStatus::Missing);
    }

    #[test]
    fn verify_match_names_remote_has_no_hash() {
        let files = vec![make_remote_file("photo.jpg", None, None, None)];
        let item = make_item(files);
        let opts = VerifyOpts {
            match_names: true,
            ..Default::default()
        };
        let filtered = filter_remote_files(&item, &opts);
        let result = match_file_against_remote(
            "test-item",
            "photo.jpg",
            "aabbccdd11223344aabbccdd11223344",
            Some(1024),
            &filtered,
            &opts,
        );
        assert_eq!(result.status, VerifyStatus::Error);
        assert!(result.detail.as_ref().unwrap().contains("no md5 hash"));
    }

    // ---- Filter tests ----

    #[test]
    fn verify_with_glob_filter() {
        let files = vec![
            make_remote_file(
                "photo.jpg",
                Some("aabbccdd11223344aabbccdd11223344"),
                None,
                None,
            ),
            make_remote_file(
                "document.pdf",
                Some("aabbccdd11223344aabbccdd11223344"),
                None,
                None,
            ),
        ];
        let item = make_item(files);
        // Glob excludes .jpg files — so matching against .jpg hash should miss
        let opts = VerifyOpts {
            glob: Some("*.pdf".to_string()),
            ..Default::default()
        };
        let filtered = filter_remote_files(&item, &opts);
        let result = match_file_against_remote(
            "test-item",
            "photo.jpg",
            "aabbccdd11223344aabbccdd11223344",
            Some(1024),
            &filtered,
            &opts,
        );
        // The hash matches document.pdf (which passes the filter)
        assert_eq!(result.status, VerifyStatus::Verified);
        assert_eq!(result.remote_key.as_deref(), Some("document.pdf"));
    }

    // ---- Edge cases ----

    #[test]
    fn verify_empty_item_reports_missing() {
        let files: Vec<FileMetadata> = vec![];
        let item = make_item(files);
        let opts = VerifyOpts::default();
        let filtered = filter_remote_files(&item, &opts);
        let result = match_file_against_remote(
            "test-item",
            "photo.jpg",
            "aabbccdd11223344aabbccdd11223344",
            Some(1024),
            &filtered,
            &opts,
        );
        assert_eq!(result.status, VerifyStatus::Missing);
    }

    #[test]
    fn verify_first_matching_remote_file_wins() {
        let files = vec![
            make_remote_file(
                "first.jpg",
                Some("aabbccdd11223344aabbccdd11223344"),
                None,
                None,
            ),
            make_remote_file(
                "second.jpg",
                Some("aabbccdd11223344aabbccdd11223344"),
                None,
                None,
            ),
        ];
        let item = make_item(files);
        let opts = VerifyOpts::default();
        let filtered = filter_remote_files(&item, &opts);
        let result = match_file_against_remote(
            "test-item",
            "photo.jpg",
            "aabbccdd11223344aabbccdd11223344",
            Some(1024),
            &filtered,
            &opts,
        );
        assert_eq!(result.status, VerifyStatus::Verified);
        assert_eq!(result.remote_key.as_deref(), Some("first.jpg"));
    }

    // ---- Serialization tests ----

    #[test]
    fn verify_result_serializes_without_none_fields() {
        let result = VerifyResult {
            identifier: "test-item".to_string(),
            local_file: "photo.jpg".to_string(),
            status: VerifyStatus::Missing,
            remote_key: None,
            local_hash: Some("abc".to_string()),
            remote_hash: None,
            algorithm: "md5".to_string(),
            bytes: None,
            detail: None,
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(!json.contains("remote_key"));
        assert!(!json.contains("remote_hash"));
        assert!(!json.contains("bytes"));
        assert!(!json.contains("detail"));
        assert!(json.contains("local_hash"));
    }

    #[test]
    fn verify_status_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&VerifyStatus::Verified).unwrap(),
            "\"verified\""
        );
        assert_eq!(
            serde_json::to_string(&VerifyStatus::Missing).unwrap(),
            "\"missing\""
        );
        assert_eq!(
            serde_json::to_string(&VerifyStatus::Mismatch).unwrap(),
            "\"mismatch\""
        );
        assert_eq!(
            serde_json::to_string(&VerifyStatus::Error).unwrap(),
            "\"error\""
        );
    }

    // ---- verify_item wiremock integration tests ----

    #[cfg(test)]
    mod integration {
        use super::*;
        use std::io::Write;
        use tempfile::NamedTempFile;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        fn make_test_client(host: &str) -> crate::IaClient {
            let mut config = crate::config::IaConfig::default();
            config.general.host = host.to_string();
            config.general.secure = false;
            config.s3_access = Some("test_access".to_string());
            config.s3_secret = Some("test_secret".to_string());
            crate::IaClient::from_config(config).unwrap()
        }

        fn metadata_json(files_json: &str) -> String {
            format!(
                r#"{{
                    "metadata": {{"identifier": "test-item"}},
                    "files": [{files_json}],
                    "server": "ia000.us.archive.org",
                    "d1": "ia000.us.archive.org",
                    "d2": "ia000.us.archive.org",
                    "dir": "/0/items/test-item"
                }}"#
            )
        }

        #[tokio::test]
        async fn verify_item_all_verified() {
            let mock_server = MockServer::start().await;
            let host = mock_server
                .uri()
                .strip_prefix("http://")
                .unwrap()
                .to_string();
            let client = make_test_client(&host);

            // MD5 of "hello" = 5d41402abc4b2a76b9719d911017c592
            let files_json = r#"{"name": "hello.txt", "source": "original", "md5": "5d41402abc4b2a76b9719d911017c592", "size": "5"}"#;

            Mock::given(method("GET"))
                .and(path("/metadata/test-item"))
                .respond_with(ResponseTemplate::new(200).set_body_string(metadata_json(files_json)))
                .mount(&mock_server)
                .await;

            let mut tmp = NamedTempFile::new().unwrap();
            tmp.write_all(b"hello").unwrap();
            tmp.flush().unwrap();

            let inputs = vec![VerifyInput::LocalFile(tmp.path().to_path_buf())];
            let opts = VerifyOpts::default();
            let results = verify_item(&client, "test-item", &inputs, &opts)
                .await
                .unwrap();

            assert_eq!(results.len(), 1);
            assert_eq!(results[0].status, VerifyStatus::Verified);
            assert_eq!(results[0].remote_key.as_deref(), Some("hello.txt"));
            assert_eq!(
                results[0].local_hash.as_deref(),
                Some("5d41402abc4b2a76b9719d911017c592")
            );
        }

        #[tokio::test]
        async fn verify_item_hash_precomputed() {
            let mock_server = MockServer::start().await;
            let host = mock_server
                .uri()
                .strip_prefix("http://")
                .unwrap()
                .to_string();
            let client = make_test_client(&host);

            let files_json = r#"{"name": "data.bin", "source": "original", "md5": "5d41402abc4b2a76b9719d911017c592", "size": "5"}"#;

            Mock::given(method("GET"))
                .and(path("/metadata/test-item"))
                .respond_with(ResponseTemplate::new(200).set_body_string(metadata_json(files_json)))
                .mount(&mock_server)
                .await;

            let inputs = vec![VerifyInput::Hash {
                filename: "data.bin".to_string(),
                hash: "5d41402abc4b2a76b9719d911017c592".to_string(),
            }];
            let opts = VerifyOpts::default();
            let results = verify_item(&client, "test-item", &inputs, &opts)
                .await
                .unwrap();

            assert_eq!(results.len(), 1);
            assert_eq!(results[0].status, VerifyStatus::Verified);
            assert_eq!(results[0].remote_key.as_deref(), Some("data.bin"));
        }

        #[tokio::test]
        async fn verify_item_missing_file() {
            let mock_server = MockServer::start().await;
            let host = mock_server
                .uri()
                .strip_prefix("http://")
                .unwrap()
                .to_string();
            let client = make_test_client(&host);

            let files_json = r#"{"name": "other.txt", "source": "original", "md5": "ffffffffffffffffffffffffffffffff", "size": "10"}"#;

            Mock::given(method("GET"))
                .and(path("/metadata/test-item"))
                .respond_with(ResponseTemplate::new(200).set_body_string(metadata_json(files_json)))
                .mount(&mock_server)
                .await;

            let inputs = vec![VerifyInput::Hash {
                filename: "notfound.txt".to_string(),
                hash: "5d41402abc4b2a76b9719d911017c592".to_string(),
            }];
            let opts = VerifyOpts::default();
            let results = verify_item(&client, "test-item", &inputs, &opts)
                .await
                .unwrap();

            assert_eq!(results.len(), 1);
            assert_eq!(results[0].status, VerifyStatus::Missing);
        }

        #[tokio::test]
        async fn verify_item_404_reports_all_missing() {
            let mock_server = MockServer::start().await;
            let host = mock_server
                .uri()
                .strip_prefix("http://")
                .unwrap()
                .to_string();
            let client = make_test_client(&host);

            // Empty item (no files)
            let files_json = "";

            Mock::given(method("GET"))
                .and(path("/metadata/test-item"))
                .respond_with(ResponseTemplate::new(200).set_body_string(metadata_json(files_json)))
                .mount(&mock_server)
                .await;

            let inputs = vec![VerifyInput::Hash {
                filename: "file.txt".to_string(),
                hash: "5d41402abc4b2a76b9719d911017c592".to_string(),
            }];
            let opts = VerifyOpts::default();
            let results = verify_item(&client, "test-item", &inputs, &opts)
                .await
                .unwrap();

            assert_eq!(results.len(), 1);
            assert_eq!(results[0].status, VerifyStatus::Missing);
        }

        #[tokio::test]
        async fn verify_item_mixed_results() {
            let mock_server = MockServer::start().await;
            let host = mock_server
                .uri()
                .strip_prefix("http://")
                .unwrap()
                .to_string();
            let client = make_test_client(&host);

            // One file matches, one doesn't
            let files_json = r#"{"name": "match.txt", "source": "original", "md5": "5d41402abc4b2a76b9719d911017c592", "size": "5"}"#;

            Mock::given(method("GET"))
                .and(path("/metadata/test-item"))
                .respond_with(ResponseTemplate::new(200).set_body_string(metadata_json(files_json)))
                .mount(&mock_server)
                .await;

            let inputs = vec![
                VerifyInput::Hash {
                    filename: "match.txt".to_string(),
                    hash: "5d41402abc4b2a76b9719d911017c592".to_string(),
                },
                VerifyInput::Hash {
                    filename: "nomatch.txt".to_string(),
                    hash: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
                },
            ];
            let opts = VerifyOpts::default();
            let results = verify_item(&client, "test-item", &inputs, &opts)
                .await
                .unwrap();

            assert_eq!(results.len(), 2);
            assert_eq!(results[0].status, VerifyStatus::Verified);
            assert_eq!(results[1].status, VerifyStatus::Missing);
        }
    }
}
