# Download Security Hardening — Implementation Plan

**Date**: 2026-03-03
**Status**: Ready for Implementation
**Audit Doc**: `docs/security/2026-03-03-download-security-audit.md`
**Parent Issue**: [#126](https://github.com/internetarchivecanada/ia/issues/126)

## Overview

Implement security fixes for the download path in `ia-core`, addressing CVE-2025-58438-equivalent vulnerabilities. This plan covers the critical and medium-priority fixes identified in the security audit.

## Phase 1: Critical Fixes (Issues #168, #172, #173)

Path traversal prevention and filename validation. These are the same class of vulnerability as CVE-2025-58438.

### Step 1.1: Add `PathTraversal` error variant

**File**: `ia-core/src/error.rs`

Add a new error variant:
```rust
#[error("path traversal blocked: {path} escapes destination directory {dest_dir}")]
PathTraversal {
    path: String,
    dest_dir: String,
},
```

Also add the JSON error code mapping for `--json` output.

### Step 1.2: Create `validate_download_path()` function

**File**: `ia-core/src/download.rs` (new helper, or a `security` submodule)

```rust
use std::path::{Component, Path, PathBuf};

/// Validate that a file name from server metadata produces a safe download path.
///
/// Rejects:
/// - Parent directory traversal (`..`)
/// - Absolute paths (`/etc/passwd`)
/// - Windows prefix paths (`C:\`)
/// - Null bytes
/// - Control characters (0x00-0x1F except tab)
///
/// Allows:
/// - Nested paths (`subdir/file.txt`) — legitimate for IA items
/// - Normal filenames
fn validate_download_path(dest_dir: &Path, file_name: &str) -> Result<PathBuf> {
    // Reject null bytes
    if file_name.contains('\0') {
        return Err(IaError::PathTraversal { ... });
    }

    // Reject control characters (except tab, which is unlikely but harmless)
    if file_name.bytes().any(|b| b < 0x20 && b != b'\t') {
        return Err(IaError::PathTraversal { ... });
    }

    // Reject empty names
    if file_name.is_empty() {
        return Err(IaError::PathTraversal { ... });
    }

    let path = Path::new(file_name);

    // Validate each component
    for component in path.components() {
        match component {
            Component::Normal(_) => {} // OK
            Component::ParentDir => return Err(IaError::PathTraversal { ... }),
            Component::RootDir | Component::Prefix(_) => return Err(IaError::PathTraversal { ... }),
            Component::CurDir => {} // "." is harmless, will be normalized
        }
    }

    let joined = dest_dir.join(file_name);

    // Belt-and-suspenders: verify the normalized path starts with dest_dir
    // This catches edge cases that component iteration might miss
    let normalized = normalize_path(&joined);
    let normalized_dest = normalize_path(dest_dir);
    if !normalized.starts_with(&normalized_dest) {
        return Err(IaError::PathTraversal { ... });
    }

    Ok(joined)
}

/// Normalize a path without requiring it to exist (unlike canonicalize).
fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => { normalized.pop(); }
            Component::CurDir => {}
            other => normalized.push(other),
        }
    }
    normalized
}
```

### Step 1.3: Integrate into `download_file()`

**File**: `ia-core/src/download.rs`

Replace line 94:
```rust
// Before (VULNERABLE):
let file_path = dest_dir.join(&file.name);

// After:
let file_path = validate_download_path(dest_dir, &file.name)?;
```

The `.part` path construction on line 159 also needs the validated path:
```rust
let part_path = PathBuf::from(format!("{}.part", file_path.display()));
```

This is already downstream of the validated `file_path`, so it's safe.

### Step 1.4: Security tests for path traversal

**File**: `ia-core/src/download.rs` (tests module)

```rust
#[test]
fn rejects_parent_traversal() {
    let dir = Path::new("/tmp/downloads");
    assert!(validate_download_path(dir, "../etc/passwd").is_err());
    assert!(validate_download_path(dir, "../../root/.bashrc").is_err());
    assert!(validate_download_path(dir, "subdir/../../etc/shadow").is_err());
}

#[test]
fn rejects_absolute_paths() {
    let dir = Path::new("/tmp/downloads");
    assert!(validate_download_path(dir, "/etc/passwd").is_err());
}

#[test]
fn rejects_null_bytes() {
    let dir = Path::new("/tmp/downloads");
    assert!(validate_download_path(dir, "file\0name.txt").is_err());
}

#[test]
fn rejects_control_characters() {
    let dir = Path::new("/tmp/downloads");
    assert!(validate_download_path(dir, "file\x01name.txt").is_err());
}

#[test]
fn allows_legitimate_nested_paths() {
    let dir = Path::new("/tmp/downloads");
    assert!(validate_download_path(dir, "subdir/test.txt").is_ok());
    assert!(validate_download_path(dir, "a/b/c/deep.txt").is_ok());
}

#[test]
fn allows_simple_filenames() {
    let dir = Path::new("/tmp/downloads");
    assert!(validate_download_path(dir, "test.txt").is_ok());
    assert!(validate_download_path(dir, "file with spaces.txt").is_ok());
}

#[tokio::test]
async fn download_rejects_traversal_filename() {
    // Integration test: verify download_file returns PathTraversal error
    let mock_server = MockServer::start().await;
    let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let file = test_file_meta("../../../etc/passwd", 100);

    let result = download_file(&client, "test-item", &file, dir.path(), &DownloadOpts::default(), None).await;
    assert!(matches!(result, Err(IaError::PathTraversal { .. })));
}
```

## Phase 2: Medium-Priority Fixes (Issues #170, #171)

### Step 2.1: Download size validation (#170)

**File**: `ia-core/src/download.rs`

Add size checking inside the download streaming loop:

```rust
let expected_size = file.size;
// ... in the streaming loop:
if let Some(expected) = expected_size {
    let max_allowed = expected + (expected / 10).max(1024); // 10% tolerance, min 1KB
    if bytes_downloaded > max_allowed {
        drop(output);
        let _ = fs::remove_file(&part_path).await;
        return Err(IaError::DownloadTooLarge {
            file: file.name.clone(),
            expected,
            received: bytes_downloaded,
        });
    }
}
```

Add `DownloadTooLarge` error variant to `error.rs`.

### Step 2.2: Redirect policy (#171)

**File**: `ia-core/src/client.rs`

Add a custom redirect policy to the reqwest client builder:

```rust
let redirect_policy = reqwest::redirect::Policy::custom(|attempt| {
    if let Some(host) = attempt.url().host_str() {
        if host == "archive.org" || host.ends_with(".archive.org") {
            return attempt.follow();
        }
    }
    attempt.error(anyhow::anyhow!(
        "redirect to non-archive.org domain blocked: {}",
        attempt.url()
    ))
});

let raw_client = reqwest::Client::builder()
    .default_headers(headers)
    .pool_max_idle_per_host(10)
    .redirect(redirect_policy)
    .build()?;
```

Test with wiremock: set up a mock that returns a 302 redirect to a non-archive.org domain, verify it's rejected.

## Phase 3: Symlink & Resume Fixes (Issue #169)

### Step 3.1: Resume TOCTOU fix

**File**: `ia-core/src/download.rs`

Replace the exists-then-metadata pattern:

```rust
// Before (TOCTOU):
let resume_from = if part_path.exists() {
    let meta = fs::metadata(&part_path).await?;
    Some(meta.len())
} else {
    None
};

// After:
let resume_from = match fs::File::open(&part_path).await {
    Ok(f) => {
        let meta = f.metadata().await?;
        // Check it's a regular file, not a symlink
        if !meta.is_file() {
            warn!(file = %file.name, "skipping resume: .part path is not a regular file");
            None
        } else {
            Some(meta.len())
        }
    }
    Err(_) => None,
};
```

### Step 3.2: Symlink detection on destination

Add symlink checks before creating directories and writing files. Use `symlink_metadata()` to detect symlinks without following them:

```rust
// Before create_dir_all, check each existing ancestor
if let Some(parent) = file_path.parent() {
    let mut check = dest_dir.to_path_buf();
    for component in parent.strip_prefix(dest_dir).unwrap_or(parent).components() {
        check.push(component);
        if check.exists() {
            let meta = fs::symlink_metadata(&check).await?;
            if meta.file_type().is_symlink() {
                return Err(IaError::PathTraversal {
                    path: check.display().to_string(),
                    dest_dir: dest_dir.display().to_string(),
                });
            }
        }
    }
}
```

## Implementation Order

```
Phase 1 (Critical):
  Step 1.1: PathTraversal error variant
  Step 1.2: validate_download_path() function
  Step 1.3: Integrate into download_file()
  Step 1.4: Security tests
  ──── All in one commit ────

Phase 2 (Medium):
  Step 2.1: Download size validation
  Step 2.2: Redirect policy
  ──── Separate commits ────

Phase 3 (Defense in depth):
  Step 3.1: Resume TOCTOU fix
  Step 3.2: Symlink detection
  ──── Separate commits ────
```

## Testing Strategy

- All fixes use wiremock mocks — zero live requests to archive.org
- Path traversal tests use `tempfile::tempdir()` for isolation
- Symlink tests create actual symlinks in temp directories
- Redirect tests use wiremock to serve 302 responses
- Size validation tests use wiremock to stream oversized responses

## Files Modified

| File | Changes |
|------|---------|
| `ia-core/src/error.rs` | Add `PathTraversal`, `DownloadTooLarge` variants + JSON error codes |
| `ia-core/src/download.rs` | `validate_download_path()`, size validation, resume TOCTOU fix, symlink detection |
| `ia-core/src/client.rs` | Custom redirect policy |
| `ia-cli/src/commands/download.rs` | No changes needed (uses ia-core functions) |

## Estimated Scope

- ~150-200 lines of new code (validation + error types)
- ~100-150 lines of new tests
- No new dependencies needed
