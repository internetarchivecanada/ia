# Download Security Audit

**Date**: 2026-03-03
**Status**: Complete — fixes implemented in [`6b98db2`](https://github.com/internetarchivecanada/ia/commit/6b98db20b87de12717d014a1af2cbd159ba898c8) (March 2026): path traversal validation, download size limits, `*.archive.org` redirect policy, symlink detection, and the resume TOCTOU fix
**Tracking Issue**: #126 in the development tracker, which was not published; issue and PR numbers in this document refer to it
**Related CVE**: [CVE-2025-58438](https://nvd.nist.gov/vuln/detail/CVE-2025-58438) (Python `internetarchive` library)

## Background

The Python `internetarchive` library had a critical directory traversal vulnerability (CVE-2025-58438, CVSS 9.4) in its `File.download()` method, fixed in v5.5.1 (September 2025). This audit examines whether the Rust port (`ia-core`) has the same or related vulnerabilities.

### CVE-2025-58438 Summary

- **Vulnerability**: `File.download()` did not sanitize filenames from server metadata or validate that the resolved download path stayed within the target directory
- **Impact**: A maliciously crafted filename (e.g., `../../etc/cron.d/backdoor`) could write files outside the intended download directory
- **CVSS**: 9.4 CRITICAL (AV:N/AC:L/PR:N/UI:P)
- **CWE**: CWE-22 (Improper Limitation of a Pathname to a Restricted Directory)
- **Advisory**: [GHSA-wx3r-v6h7-frjp](https://github.com/advisories/GHSA-wx3r-v6h7-frjp)
- **Fix commit**: [`cba2d45`](https://github.com/jjjake/internetarchive/commit/cba2d459e10a9489fb35caeba0b03e80f5f5d7c2)
- **Discoverer**: Pengo Wray ([pengowray](https://github.com/pengowray))
- **Distribution advisories**: Ubuntu USN-7989-1, Debian DSA-6035-1, Debian LTS DLA-4314-1

### Python Library Fix (Three Commits)

1. **`d324f30`** (Aug 19, 2025) — Added `sanitize_filename()` / `sanitize_filepath()` with platform-specific rules:
   - POSIX: percent-encode forward slashes
   - Windows: percent-encode `<>:"/\|?*`, control chars (`\x00-\x1F`), trailing dots/spaces, reserved device names (CON, NUL, COM1-9, LPT1-9)

2. **`d583bd5`** (Sep 4, 2025) — Added directory traversal guard:
   ```python
   target_path = Path(file_path).resolve()
   base_dir = Path(destdir).resolve() if destdir else Path.cwd().resolve()
   target_path.relative_to(base_dir)  # raises ValueError if outside
   ```

3. **`c4526f8`** (Sep 15, 2025) — Added `DirectoryTraversalError` exception, `is_path_within_directory()` utility, Windows device name protection, long filename warnings

### Python Library: Redirect Auth Handling

The Python library's `ArchiveSession` overrides `rebuild_auth()` to preserve S3 auth headers on redirects to `*.archive.org` domains while stripping them for other domains:

```python
def rebuild_auth(self, prepared_request, response):
    u = urlparse(prepared_request.url)
    if u.netloc.endswith('archive.org'):
        return  # Keep auth headers for archive.org redirects
    super().rebuild_auth(prepared_request, response)  # Strip auth for non-archive.org
```

This predated the CVE and wasn't changed, but represents defense-in-depth relevant to our codebase.

## Rust Port Assessment

### Vulnerability Summary

| # | Vulnerability | Severity | Status | Location |
|---|--------------|----------|--------|----------|
| 1 | Path traversal (arbitrary file write) | **CRITICAL** | Vulnerable | `download.rs:94` |
| 2 | Symlink attacks (TOCTOU) | **CRITICAL** | Vulnerable | `download.rs:98,160-165,277` |
| 3 | No download size validation | **MEDIUM** | Vulnerable | `download.rs:227-245` |
| 4 | Redirect target not validated | **MEDIUM** | Vulnerable | `client.rs` (no policy) |
| 5 | No filename sanitization | **MEDIUM** | Vulnerable | `types.rs:91` |
| 6 | Resume logic race condition | **MEDIUM** | Vulnerable | `download.rs:160-165` |
| 7 | File permissions (umask) | **LOW** | Minor concern | `download.rs:98` |
| 8 | Integer overflow in progress | **LOW** | Not practical | `download.rs:230` |

### Detailed Analysis

#### 1. CRITICAL: Path Traversal (Arbitrary File Write)

**Location**: `ia-core/src/download.rs:94`

```rust
let file_path = dest_dir.join(&file.name);
```

`file.name` comes directly from deserialized server JSON (`FileMetadata.name` in `types.rs:91`). No validation is performed. A name like `../../.bashrc` would resolve outside `dest_dir`. Line 98 compounds the issue:

```rust
fs::create_dir_all(parent).await?;
```

This creates any intermediate directories needed for the traversal path.

**This is the exact same vulnerability as CVE-2025-58438.**

**Fix**: Validate that the canonical path stays within `dest_dir`:
```rust
let file_path = dest_dir.join(&file.name);
let canonical = file_path.canonicalize()?; // or use dunce::canonicalize on Windows
if !canonical.starts_with(dest_dir.canonicalize()?) {
    return Err(IaError::PathTraversal { ... });
}
```

Note: `canonicalize()` requires the path to exist, so we must validate *before* creating directories. Use component-level validation instead:
```rust
fn validate_download_path(dest_dir: &Path, file_name: &str) -> Result<PathBuf> {
    let path = Path::new(file_name);
    for component in path.components() {
        match component {
            Component::ParentDir => return Err(IaError::PathTraversal { ... }),
            Component::RootDir | Component::Prefix(_) => return Err(IaError::PathTraversal { ... }),
            _ => {}
        }
    }
    // Also check for null bytes
    if file_name.contains('\0') {
        return Err(IaError::PathTraversal { ... });
    }
    Ok(dest_dir.join(file_name))
}
```

#### 2. CRITICAL: Symlink Attacks (TOCTOU)

**Locations**: `ia-core/src/download.rs:98, 160-165, 214-221, 277`

No symlink detection anywhere in the download path:
- `create_dir_all(parent)` follows symlinks in the path — an attacker could plant a symlink at any directory level
- `.part` file existence check (line 160) then open (line 214-221) is a TOCTOU race — between check and open, the `.part` file could be replaced with a symlink
- `fs::rename(&part_path, &file_path)` follows symlinks — the final rename could overwrite a symlinked target
- Resume logic reads metadata of a potentially-replaced `.part` file

**Fix**:
- Use `symlink_metadata()` instead of `metadata()` to detect symlinks
- Open files with `O_NOFOLLOW` equivalent (via `OpenOptions` + platform-specific flags)
- Verify destination path components are not symlinks before creating directories
- Get metadata from file descriptor after opening, not from path

#### 3. MEDIUM: No Download Size Validation

**Location**: `ia-core/src/download.rs:227-245`

Response streaming has no bounds checking. The code downloads until the stream ends with no limit:

```rust
while let Some(chunk) = stream.next().await {
    let chunk = chunk.map_err(reqwest_middleware::Error::from)?;
    output.write_all(&chunk).await?;
    bytes_downloaded += chunk.len() as u64;
    // ...
}
```

A malicious server (or compromised redirect) could send an arbitrarily large response, exhausting disk space.

**Fix**: Compare `Content-Length` against `FileMetadata.size` when available. Abort download if `bytes_downloaded` exceeds `expected_size * 1.1` (10% tolerance for encoding differences).

#### 4. MEDIUM: Redirect Target Not Validated

**Location**: `ia-core/src/client.rs` — `reqwest::Client::builder()` uses default redirect policy

The default reqwest policy follows up to 10 redirects to **any domain**. No restriction to `*.archive.org`.

Current risk is limited because we don't send auth headers on downloads. However:
- Unrestricted redirects enable SSRF attacks
- If auth headers are added in the future (for restricted items), credentials could leak to arbitrary domains
- A compromised IA server could redirect downloads to a malicious file server

The Python library handles this via `rebuild_auth()` — preserving auth only for `*.archive.org` redirects.

**Fix**: Configure `reqwest::redirect::Policy::custom()` to:
1. Only follow redirects to `*.archive.org` domains
2. Strip any auth headers when redirecting to non-archive.org domains

#### 5. MEDIUM: No Filename Sanitization

**Location**: `ia-core/src/types.rs:91`

```rust
pub struct FileMetadata {
    pub name: String,  // No validation
```

`FileMetadata.name` accepts any string from the server. No rejection of:
- Absolute paths (`/etc/passwd`)
- Traversal components (`../`)
- Null bytes (`\0`)
- Control characters
- Platform-specific reserved names (Windows: `CON`, `NUL`, `COM1`, `LPT1`, etc.)

Note: IA legitimately uses nested paths (e.g., `subdir/file.txt`), so forward slashes are valid. The sanitization must preserve legitimate nesting while rejecting malicious patterns.

**Fix**: Create a `sanitize_filename()` utility that validates path components without breaking legitimate nested paths. Consider the Python library's regression (issue #717) where `os.path.dirname()` mishandled colons on Windows.

#### 6. MEDIUM: Resume Logic Race Condition

**Location**: `ia-core/src/download.rs:160-165`

```rust
let resume_from = if part_path.exists() {
    let meta = fs::metadata(&part_path).await?;
    Some(meta.len())
} else {
    None
};
```

Between `exists()` and `metadata()`, the `.part` file could be replaced (symlink swap, file replacement). An attacker with local access could redirect the append to a sensitive file.

**Fix**: Open the file first, then get metadata from the file descriptor:
```rust
let resume_from = match fs::OpenOptions::new().read(true).open(&part_path).await {
    Ok(f) => {
        let meta = f.metadata().await?;
        Some(meta.len())
    }
    Err(_) => None,
};
```

#### 7. LOW-MEDIUM: File Permissions

**Location**: `ia-core/src/download.rs:98`

Directories created via `create_dir_all` use default umask. In shared environments (multi-user servers), this could enable other users to read downloaded files or plant files in download directories.

#### 8. LOW: Integer Overflow in Progress Tracking

**Location**: `ia-core/src/download.rs:230`

```rust
bytes_downloaded += chunk.len() as u64;
```

Progress counters use `u64` (max ~18 exabytes). Not a practical risk.

### What's Currently Done Right

- **Checksum verification**: Optional MD5 verification (lines 251-274)
- **URL encoding**: Filenames are properly encoded for HTTP requests (`urlencoding::encode`, line 155)
- **Atomic writes**: `.part` → final rename pattern prevents partial file exposure (line 277)
- **Concurrency control**: Semaphore-based limiting prevents resource exhaustion
- **Error propagation**: Proper Rust error handling with `?` operator
- **Retry with backoff**: Exponential backoff with retry budget
- **Resume corruption prevention**: Detects when server ignores Range header (200 vs 206, lines 198-204)
- **Non-retryable errors**: 403/404 fail immediately without wasting retry budget

### Comparison with Python Library

| Defense | Python (post-fix) | Rust (current) |
|---------|-------------------|----------------|
| Path traversal check | `Path.resolve().relative_to(base_dir)` | **Missing** |
| Filename sanitization | `sanitize_filename()` per-platform | **Missing** |
| `DirectoryTraversalError` | Custom exception | **Missing** |
| `is_path_within_directory()` | `resolve()` + `relative_to()` | **Missing** |
| Windows device name protection | CON, NUL, etc. blocked | **Missing** |
| `rebuild_auth()` redirect guard | Preserves auth only to `*.archive.org` | No redirect policy |
| Symlink detection | Not addressed in CVE fix | **Missing** |
| Content-Length validation | Not addressed in CVE fix | **Missing** |
| MD5 checksum verification | Yes | Yes |
| URL encoding | Yes | Yes |
| Atomic .part rename | Yes | Yes |

## Priority Matrix

### Priority 1: Critical (Must fix before any release)
- Path traversal prevention (`validate_download_path`)
- Path component validation (reject `..`, absolute paths, null bytes)
- Security-focused tests

### Priority 2: Important (Fix before auth features)
- Redirect policy (restrict to `*.archive.org`)
- Download size validation
- Filename sanitization utility
- `rebuild_auth` equivalent (needed when adding auth to downloads)

### Priority 3: Defense in Depth
- Symlink detection and `O_NOFOLLOW`
- Resume TOCTOU fix (open-then-stat)
- Platform-specific sanitization (Windows device names)

## Future Feature Security Notes

### When Implementing Upload Support
- Same path validation needed in reverse: ensure we don't accidentally read files outside intended directories
- Validate that user-provided file paths don't escape the source directory
- Consider symlink following behavior for uploads (should we follow symlinks or reject them?)

### When Implementing Authentication
- **MUST** implement redirect-aware auth stripping (like Python's `rebuild_auth`)
- Never send `Authorization: LOW` headers to non-`*.archive.org` domains
- Consider: should auth headers be sent on initial download requests, or only after a 401 challenge?
- Cookie handling: domain-scope cookies to `.archive.org` only

### When Implementing Delete
- The Python library had a catastrophic bug in v5.4.1 where `ia delete --glob` deleted ALL files regardless of pattern (fixed in v5.7.0). Our Rust implementation must have thorough tests for glob/format filtering in destructive operations.

## References

- [CVE-2025-58438 (NVD)](https://nvd.nist.gov/vuln/detail/CVE-2025-58438)
- [GHSA-wx3r-v6h7-frjp (GitHub Advisory)](https://github.com/advisories/GHSA-wx3r-v6h7-frjp)
- [Fix commit cba2d45](https://github.com/jjjake/internetarchive/commit/cba2d459e10a9489fb35caeba0b03e80f5f5d7c2)
- [Python library issue #717 (regression from fix)](https://github.com/jjjake/internetarchive/issues/717)
- [Snyk advisory](https://security.snyk.io/vuln/SNYK-PYTHON-INTERNETARCHIVE-12549189)
