# Batch Interface Review Fixes — Implementation Plan

**Goal:** Fix 3 issues found in PR #269 review: consolidate identifier operations into `ia-core/src/identifier.rs`, change tasks submit `cmd` to `--cmd` flag, and make JSONL parsing consistent across all commands.

**Architecture:** Move `validate_identifier`, `sanitize_identifier`, `generate_identifier`, and `parse_identifier_line` from 3 scattered locations into a new `ia-core/src/identifier.rs` module. Update all callers. Change tasks submit to use `--cmd` flag (eliminating heuristic normalization). Use `parse_identifier_line` in all `collect_identifiers` functions.

**Tech Stack:** Rust, clap (CLI args), serde_json (JSONL parsing), cargo test

**Spec:** `docs/plans/2026-03-17-batch-interface-review-fixes.md`

---

## File Structure

| File | Responsibility |
|------|----------------|
| `ia-core/src/identifier.rs` | **NEW** — All identifier operations: validate, sanitize, generate, parse from line. Unit tests. |
| `ia-core/src/lib.rs` | Add `pub mod identifier;` |
| `ia-core/src/upload/validate.rs` | Remove `validate_identifier` + its 6 tests. Keep `validate_required_metadata`, `check_collections`, `validate_file`. |
| `ia-core/src/upload/template.rs` | Remove `sanitize_identifier`, `generate_identifier` + 7 sanitize tests. Import from `crate::identifier`. Adapt `generate_template` call site. |
| `ia-cli/src/commands/tasks.rs` | Replace `cmd` positional with `--cmd` flag. Delete `normalize_submit_args`, `looks_like_task_cmd`. Use `parse_identifier_line` in `collect_submit_identifiers`. |
| `ia-cli/src/commands/download.rs` | Remove local `parse_identifier_line` + 6 tests. Import from `ia_core::identifier`. |
| `ia-cli/src/commands/metadata.rs` | Use `parse_identifier_line` in `collect_identifiers_from_batch` and `collect_identifiers_from_export`. |
| `ia-cli/src/commands/ai.rs` | Use `parse_identifier_line` in `collect_identifiers`. |
| `ia-core/src/upload/item.rs` | Update import. |
| `ia-core/src/upload/batch.rs` | Update import. |
| `ia-core/src/collection.rs` | Update import. |
| `ia-cli/tests/tasks.rs` | Update all submit tests to `--cmd`. Delete deprecated test. Add new tests. |
| `docs/usage.md` | Update tasks submit docs. |

---

### Task 1: Create `ia-core/src/identifier.rs` with `validate_identifier`

**Files:**
- Create: `ia-core/src/identifier.rs`
- Modify: `ia-core/src/lib.rs:1` (add module declaration)

- [ ] **Step 1: Create `ia-core/src/identifier.rs` with `validate_identifier` and its tests**

Move `validate_identifier` from `ia-core/src/upload/validate.rs:19-57` into a new file `ia-core/src/identifier.rs`. Include the doc comment with updated doc-test path. Move the 6 tests from `validate.rs:140-180`.

```rust
use crate::error::IaError;

/// Validate an IA identifier.
///
/// Rules: 3-100 chars, `[a-zA-Z0-9._-@]`, must start with alphanumeric or `@`.
///
/// # Examples
///
/// ```
/// use ia_core::identifier::validate_identifier;
///
/// assert!(validate_identifier("nasa").is_ok());
/// assert!(validate_identifier("@username").is_ok());
/// assert!(validate_identifier("ab").is_err()); // too short
/// assert!(validate_identifier("has space").is_err()); // invalid char
/// ```
pub fn validate_identifier(id: &str) -> Result<(), IaError> {
    if id.is_empty() || id.len() < 3 {
        return Err(IaError::InvalidIdentifier {
            identifier: id.to_string(),
            reason: "must be at least 3 characters".into(),
        });
    }
    if id.len() > 100 {
        return Err(IaError::InvalidIdentifier {
            identifier: id.to_string(),
            reason: "must be at most 100 characters".into(),
        });
    }

    let Some(first) = id.chars().next() else {
        return Err(IaError::InvalidIdentifier {
            identifier: id.to_string(),
            reason: "identifier is empty".into(),
        });
    };
    if !first.is_ascii_alphanumeric() && first != '@' {
        return Err(IaError::InvalidIdentifier {
            identifier: id.to_string(),
            reason: format!("must start with alphanumeric or '@', got '{first}'"),
        });
    }

    if let Some(bad) = id
        .chars()
        .find(|c| !matches!(c, 'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '_' | '-' | '@'))
    {
        return Err(IaError::InvalidIdentifier {
            identifier: id.to_string(),
            reason: format!("contains invalid character '{bad}'"),
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- validate_identifier tests --

    #[test]
    fn valid_identifiers() {
        assert!(validate_identifier("nasa").is_ok());
        assert!(validate_identifier("my-item-123").is_ok());
        assert!(validate_identifier("test.item").is_ok());
        assert!(validate_identifier("a_b_c").is_ok());
        assert!(validate_identifier("abc").is_ok());
        assert!(validate_identifier("@username").is_ok());
    }

    #[test]
    fn invalid_identifier_too_short() {
        assert!(validate_identifier("ab").is_err());
        assert!(validate_identifier("").is_err());
    }

    #[test]
    fn invalid_identifier_too_long() {
        let long = "a".repeat(101);
        assert!(validate_identifier(&long).is_err());
    }

    #[test]
    fn invalid_identifier_bad_chars() {
        assert!(validate_identifier("has space").is_err());
        assert!(validate_identifier("has!bang").is_err());
        assert!(validate_identifier("has#hash").is_err());
    }

    #[test]
    fn invalid_identifier_bad_start() {
        assert!(validate_identifier(".dotstart").is_err());
        assert!(validate_identifier("_understart").is_err());
        assert!(validate_identifier("-dashstart").is_err());
    }

    #[test]
    fn valid_identifier_at_max_length() {
        let exactly_100 = "a".repeat(100);
        assert!(validate_identifier(&exactly_100).is_ok());
    }
}
```

- [ ] **Step 2: Register the module in `lib.rs`**

Add `pub mod identifier;` to `ia-core/src/lib.rs` (alphabetically between `files` and `joblog`).

- [ ] **Step 3: Run tests to verify the new module compiles and tests pass**

Run: `cargo test -p ia-core identifier`
Expected: 6 tests pass

- [ ] **Step 4: Remove `validate_identifier` and its 6 tests from `upload/validate.rs`**

Remove function at lines 5-57 and tests at lines 138-180 from `ia-core/src/upload/validate.rs`. Remove the now-unused `use crate::error::IaError;` line ONLY if nothing else in the file uses it (check: `validate_required_metadata` returns `Result<(), IaError>`, so the import stays). Also remove `use std::path::Path;` ONLY if `validate_file` doesn't use it (it does — keep it).

Keep: `validate_required_metadata`, `check_collections`, `validate_file`, and all their tests.

- [ ] **Step 5: Update callers of `validate_identifier` to new path**

In `ia-core/src/upload/item.rs:10`: change `use crate::upload::validate::{validate_file, validate_identifier, validate_required_metadata};` to `use crate::upload::validate::{validate_file, validate_required_metadata};` and add `use crate::identifier::validate_identifier;`

In `ia-core/src/upload/batch.rs:12`: change `use crate::upload::validate::{validate_file, validate_identifier};` to `use crate::upload::validate::validate_file;` and add `use crate::identifier::validate_identifier;`

In `ia-core/src/collection.rs:6`: change `use crate::upload::validate::validate_identifier;` to `use crate::identifier::validate_identifier;`

- [ ] **Step 6: Run full ia-core tests**

Run: `cargo test -p ia-core`
Expected: All ia-core tests pass (upload, collection, identifier)

- [ ] **Step 7: Commit**

```
git add ia-core/src/identifier.rs ia-core/src/lib.rs ia-core/src/upload/validate.rs ia-core/src/upload/item.rs ia-core/src/upload/batch.rs ia-core/src/collection.rs
git commit -m "refactor: move validate_identifier to ia-core/src/identifier.rs

Create dedicated identifier module consolidating identifier operations.
Start with validate_identifier moved from upload/validate.rs. Update
all callers: upload/item.rs, upload/batch.rs, collection.rs.
```

---

### Task 2: Move `sanitize_identifier` and `generate_identifier` to `identifier.rs`

**Files:**
- Modify: `ia-core/src/identifier.rs`
- Modify: `ia-core/src/upload/template.rs`

- [ ] **Step 1: Add `sanitize_identifier` to `identifier.rs`**

Copy `sanitize_identifier` from `ia-core/src/upload/template.rs:31-60`. Make it `pub`. Add the 7 sanitize tests.

```rust
/// Sanitize a string into a valid IA identifier component.
///
/// - Lowercases ASCII alphanumeric characters
/// - Replaces non-allowed characters with `-`
/// - Strips leading non-alphanumeric characters
/// - Returns empty string if result is less than 3 chars
/// - Truncates to 100 chars
pub fn sanitize_identifier(s: &str) -> String {
    let sanitized: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    // Ensure starts with alphanumeric
    let sanitized = sanitized
        .trim_start_matches(|c: char| !c.is_ascii_alphanumeric())
        .to_string();
    if sanitized.len() < 3 {
        String::new() // too short, leave for user to fill in
    } else if sanitized.len() > 100 {
        sanitized[..100].to_string()
    } else {
        sanitized
    }
}
```

Add the 7 tests to the `tests` module:

```rust
    // -- sanitize_identifier tests --

    #[test]
    fn sanitize_simple() {
        assert_eq!(sanitize_identifier("Hello World"), "hello-world");
    }

    #[test]
    fn sanitize_strips_leading_non_alnum() {
        assert_eq!(sanitize_identifier("--my-file"), "my-file");
    }

    #[test]
    fn sanitize_too_short() {
        assert_eq!(sanitize_identifier("ab"), "");
    }

    #[test]
    fn sanitize_truncates_long() {
        let long = "a".repeat(150);
        assert_eq!(sanitize_identifier(&long).len(), 100);
    }

    #[test]
    fn sanitize_preserves_dots_and_underscores() {
        assert_eq!(sanitize_identifier("my_file.v2"), "my_file.v2");
    }

    #[test]
    fn sanitize_empty_input() {
        assert_eq!(sanitize_identifier(""), "");
    }

    #[test]
    fn sanitize_all_special_chars() {
        assert_eq!(sanitize_identifier("@#$"), "");
    }
```

- [ ] **Step 2: Add `generate_identifier` to `identifier.rs`**

Add `use std::path::Path;` to the imports at the top of `identifier.rs` (needed by `generate_identifier`).

Move `generate_identifier` from `ia-core/src/upload/template.rs:125-153`. Change signature from `(&Path, &TemplateOpts)` to explicit parameters. Make it `pub`.

```rust
/// Generate an identifier for a file based on options.
///
/// Derives an identifier from the file path by extracting the filename stem
/// (if `from_filename` is true) or parent directory name (if `from_dirname`
/// is true), sanitizing the result, and optionally prepending a prefix.
///
/// Returns an empty string if neither `from_filename` nor `from_dirname` is
/// set, or if the sanitized result is too short (< 3 chars).
pub fn generate_identifier(
    path: &Path,
    prefix: Option<&str>,
    from_filename: bool,
    from_dirname: bool,
) -> String {
    let raw = if from_filename {
        // Use filename without extension
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string()
    } else if from_dirname {
        // Use parent directory name
        path.parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string()
    } else {
        return String::new();
    };

    let sanitized = sanitize_identifier(&raw);
    if sanitized.is_empty() {
        return String::new();
    }

    match prefix {
        Some(prefix) => format!("{prefix}-{sanitized}"),
        None => sanitized,
    }
}
```

Add new direct unit tests for `generate_identifier`:

```rust
    // -- generate_identifier tests --

    #[test]
    fn generate_from_filename() {
        let path = Path::new("/tmp/My Document.pdf");
        assert_eq!(generate_identifier(path, None, true, false), "my-document");
    }

    #[test]
    fn generate_from_dirname() {
        let path = Path::new("/tmp/My Collection/file.txt");
        assert_eq!(
            generate_identifier(path, None, false, true),
            "my-collection"
        );
    }

    #[test]
    fn generate_with_prefix() {
        let path = Path::new("/tmp/file.txt");
        assert_eq!(
            generate_identifier(path, Some("myproject"), true, false),
            "myproject-file"
        );
    }

    #[test]
    fn generate_neither_mode_returns_empty() {
        let path = Path::new("/tmp/file.txt");
        assert_eq!(generate_identifier(path, None, false, false), "");
    }

    #[test]
    fn generate_prefix_without_mode_returns_empty() {
        let path = Path::new("/tmp/file.txt");
        assert_eq!(
            generate_identifier(path, Some("prefix"), false, false),
            ""
        );
    }

    #[test]
    fn generate_short_name_returns_empty() {
        let path = Path::new("/tmp/ab.txt");
        assert_eq!(generate_identifier(path, None, true, false), "");
    }

    #[test]
    fn generate_root_path_dirname_returns_empty() {
        // Path with no meaningful parent directory name
        let path = Path::new("/file.txt");
        // parent is "/", file_name of "/" is None
        assert_eq!(generate_identifier(path, None, false, true), "");
    }
```

- [ ] **Step 3: Add coherence test**

```rust
    #[test]
    fn sanitize_output_passes_validate_or_is_empty() {
        let inputs = ["Hello World", "test", "a b c d e", "@#$", "ab", "", "valid-id"];
        for input in inputs {
            let sanitized = sanitize_identifier(input);
            if !sanitized.is_empty() {
                assert!(
                    validate_identifier(&sanitized).is_ok(),
                    "sanitize_identifier({input:?}) = {sanitized:?} should pass validation"
                );
            }
        }
    }
```

- [ ] **Step 4: Run identifier tests**

Run: `cargo test -p ia-core identifier`
Expected: All identifier tests pass (6 validate + 7 sanitize + 7 generate + 1 coherence = 21)

- [ ] **Step 5: Remove from `template.rs` and update call site**

Remove `sanitize_identifier` (lines 31-60) and `generate_identifier` (lines 125-153) from `ia-core/src/upload/template.rs`. Remove the 7 sanitize tests (lines 207-241).

Add import at top: `use crate::identifier::{generate_identifier, sanitize_identifier};`

Note: `sanitize_identifier` import is needed because it's still referenced by... actually, check: is `sanitize_identifier` called anywhere else in template.rs? Only by `generate_identifier`, which is also being moved. So the import for `sanitize_identifier` is not needed in template.rs. Only `generate_identifier` is called by `generate_template`.

Update `generate_template` call site at line 74 from:
```rust
let identifier = generate_identifier(&path, opts);
```
to:
```rust
let identifier = generate_identifier(
    &path,
    opts.identifier_prefix.as_deref(),
    opts.identifier_from_filename,
    opts.identifier_from_dirname,
);
```

- [ ] **Step 6: Run full ia-core tests**

Run: `cargo test -p ia-core`
Expected: All tests pass (template tests still exercise `generate_identifier` indirectly through `generate_template`)

- [ ] **Step 7: Commit**

```
git add ia-core/src/identifier.rs ia-core/src/upload/template.rs
git commit -m "refactor: move sanitize/generate_identifier to identifier.rs

Move sanitize_identifier (made pub) and generate_identifier from
upload/template.rs to the identifier module. Change generate_identifier
signature from (&Path, &TemplateOpts) to (&Path, Option<&str>, bool,
bool) for decoupling from template types.

Add 7 direct unit tests for generate_identifier and a coherence test
verifying sanitize output always passes validate.
```

---

### Task 3: Move `parse_identifier_line` to `identifier.rs`

**Files:**
- Modify: `ia-core/src/identifier.rs`
- Modify: `ia-cli/src/commands/download.rs`

- [ ] **Step 1: Add `parse_identifier_line` to `identifier.rs`**

Copy from `ia-cli/src/commands/download.rs:122-143`. Make it `pub`. Move the 6 tests. Add 1 new test for malformed JSON.

```rust
/// Extract an identifier from a line, handling both plain text and JSONL formats.
///
/// Supports:
///   - Plain identifier: `my-item-id`
///   - JSONL from `ia search --json`: `{"identifier": "my-item-id", ...}`
///   - Empty lines and comment lines (starting with `#`): returns `None`
///
/// For JSON objects without an `"identifier"` field, returns the raw line.
/// For malformed JSON (starts with `{` but fails to parse), returns the raw line.
pub fn parse_identifier_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }

    // Try to parse as JSON if it looks like a JSON object
    if trimmed.starts_with('{') {
        if let Ok(obj) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Some(id) = obj.get("identifier").and_then(|v| v.as_str()) {
                return Some(id.to_string());
            }
        }
    }

    Some(trimmed.to_string())
}
```

Add the 6 moved tests plus 1 new test to the `tests` module:

```rust
    // -- parse_identifier_line tests --

    #[test]
    fn parse_plain_identifier() {
        assert_eq!(
            parse_identifier_line("nasa"),
            Some("nasa".to_string())
        );
    }

    #[test]
    fn parse_plain_identifier_with_whitespace() {
        assert_eq!(
            parse_identifier_line("  nasa  "),
            Some("nasa".to_string())
        );
    }

    #[test]
    fn parse_jsonl_identifier() {
        assert_eq!(
            parse_identifier_line(r#"{"identifier": "cubanc_000418"}"#),
            Some("cubanc_000418".to_string())
        );
    }

    #[test]
    fn parse_jsonl_with_extra_fields() {
        assert_eq!(
            parse_identifier_line(
                r#"{"identifier": "nasa", "title": "NASA Images", "mediatype": "image"}"#
            ),
            Some("nasa".to_string())
        );
    }

    #[test]
    fn parse_empty_and_comment_lines() {
        assert_eq!(parse_identifier_line(""), None);
        assert_eq!(parse_identifier_line("  "), None);
        assert_eq!(parse_identifier_line("# comment"), None);
    }

    #[test]
    fn parse_json_without_identifier_field() {
        // JSON object without "identifier" — use raw line as fallback
        assert_eq!(
            parse_identifier_line(r#"{"title": "something"}"#),
            Some(r#"{"title": "something"}"#.to_string())
        );
    }

    #[test]
    fn parse_malformed_json() {
        // Starts with '{' but is not valid JSON — use raw line as fallback
        assert_eq!(
            parse_identifier_line(r#"{"identifier": "broken"#),
            Some(r#"{"identifier": "broken"#.to_string())
        );
    }
```

Note: `identifier.rs` needs `serde_json` as a dependency. Check `ia-core/Cargo.toml` — `serde_json` is already a dependency.

- [ ] **Step 2: Run identifier tests**

Run: `cargo test -p ia-core identifier`
Expected: All 28 tests pass (6 validate + 7 sanitize + 7 generate + 1 coherence + 7 parse = 28). Note: the 7 parse tests include the 1 new malformed JSON test.

- [ ] **Step 3: Remove `parse_identifier_line` and its tests from `download.rs`**

Remove function at `ia-cli/src/commands/download.rs:122-143` and tests at lines 655-700.

Add import: `use ia_core::identifier::parse_identifier_line;`

The existing `collect_identifiers` function already calls `parse_identifier_line` — it just needs the import.

- [ ] **Step 4: Run download tests**

Run: `cargo test -p ia-cli download`
Expected: All download tests pass

- [ ] **Step 5: Commit**

```
git add ia-core/src/identifier.rs ia-cli/src/commands/download.rs
git commit -m "refactor: move parse_identifier_line to identifier.rs

Move JSONL-aware identifier line parsing from ia-cli/download.rs to
ia-core/src/identifier.rs. This makes it available to all commands
that need to parse identifiers from stdin or itemlist files.

Add test for malformed JSON fallback behavior.
```

---

### Task 4: Use `parse_identifier_line` in metadata, tasks, and ai

**Files:**
- Modify: `ia-cli/src/commands/metadata.rs`
- Modify: `ia-cli/src/commands/tasks.rs`
- Modify: `ia-cli/src/commands/ai.rs`

- [ ] **Step 1: Fix `collect_identifiers_from_batch` in `metadata.rs`**

Add import at top of file: `use ia_core::identifier::parse_identifier_line;`

Replace the `--itemlist` file loop (lines 1613-1621):
```rust
// OLD:
for line in content.lines() {
    let trimmed = line.trim();
    if !trimmed.is_empty() && !trimmed.starts_with('#') {
        ids.push(trimmed.to_string());
    }
}
```
with:
```rust
// NEW:
for line in content.lines() {
    if let Some(id) = parse_identifier_line(line) {
        ids.push(id);
    }
}
```

Replace the stdin loop (lines 1639-1647):
```rust
// OLD:
let trimmed = line.trim().to_string();
if !trimmed.is_empty() && !trimmed.starts_with('#') {
    ids.push(trimmed);
}
```
with:
```rust
// NEW:
if let Some(id) = parse_identifier_line(&line) {
    ids.push(id);
}
```

- [ ] **Step 2: Fix `collect_identifiers_from_export` in `metadata.rs`**

Replace only the stdin loop (lines 864-873). The `--itemlist` path uses `read_identifiers_from_file` which is unchanged.

```rust
// OLD:
let trimmed = line.trim().to_string();
if !trimmed.is_empty() && !trimmed.starts_with('#') {
    ids.push(trimmed);
}
```
with:
```rust
// NEW:
if let Some(id) = parse_identifier_line(&line) {
    ids.push(id);
}
```

- [ ] **Step 3: Fix `collect_submit_identifiers` in `tasks.rs`**

Add import at top: `use ia_core::identifier::parse_identifier_line;`

Replace `--itemlist` file loop (lines 1186-1194):
```rust
// OLD:
for line in content.lines() {
    let trimmed = line.trim();
    if !trimmed.is_empty() && !trimmed.starts_with('#') {
        ids.push(trimmed.to_string());
    }
}
```
with:
```rust
// NEW:
for line in content.lines() {
    if let Some(id) = parse_identifier_line(line) {
        ids.push(id);
    }
}
```

Replace the stdin block (lines 1218-1228) — remove the inline JSONL parsing:
```rust
// OLD:
let trimmed = line.trim();
if !trimmed.is_empty() && !trimmed.starts_with('#') {
    // Try JSONL: extract "identifier" field
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if let Some(id) = value.get("identifier").and_then(|v| v.as_str()) {
            ids.push(id.to_string());
            continue;
        }
    }
    ids.push(trimmed.to_string());
}
```
with:
```rust
// NEW:
if let Some(id) = parse_identifier_line(&line) {
    ids.push(id);
}
```

- [ ] **Step 4: Fix `collect_identifiers` in `ai.rs`**

Add import at top: `use ia_core::identifier::parse_identifier_line;`

Replace `--itemlist` file loop (lines 390-394):
```rust
// OLD:
let trimmed = line.trim();
if !trimmed.is_empty() && !trimmed.starts_with('#') {
    ids.push(trimmed.to_string());
}
```
with:
```rust
// NEW:
if let Some(id) = parse_identifier_line(line) {
    ids.push(id);
}
```

Replace the stdin block (lines 417-429) — remove inline JSONL parsing:
```rust
// OLD:
let trimmed = line.trim();
if trimmed.is_empty() || trimmed.starts_with('#') {
    continue;
}
// Try JSONL: extract "identifier" field
if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
    if let Some(id) = value.get("identifier").and_then(|v| v.as_str()) {
        ids.push(id.to_string());
        continue;
    }
}
ids.push(trimmed.to_string());
```
with:
```rust
// NEW:
if let Some(id) = parse_identifier_line(&line) {
    ids.push(id);
}
```

- [ ] **Step 5: Run all tests**

Run: `cargo test -p ia-cli`
Expected: All CLI tests pass

- [ ] **Step 6: Commit**

```
git add ia-cli/src/commands/metadata.rs ia-cli/src/commands/tasks.rs ia-cli/src/commands/ai.rs
git commit -m "fix: use shared parse_identifier_line in all commands

Replace inline/plain-text identifier parsing in metadata, tasks, and
ai commands with the shared parse_identifier_line from ia_core::identifier.

This fixes: ia search --json | ia metadata modify silently using raw
JSON strings as identifiers. All commands now consistently handle both
plain text and JSONL input for --itemlist files and stdin.
```

---

### Task 5: Change tasks submit `cmd` to `--cmd` flag

**Files:**
- Modify: `ia-cli/src/commands/tasks.rs`

- [ ] **Step 1: Change `SubmitArgs.cmd` from positional to `--cmd` flag**

In `ia-cli/src/commands/tasks.rs`, change the `cmd` field in `SubmitArgs` (line 174-176) from:
```rust
/// Task command (e.g. derive, make_dark)
#[arg()]
pub cmd: Option<String>,
```
to:
```rust
/// Task command (e.g. derive, make_dark)
#[arg(long, required_unless_present = "spreadsheet")]
pub cmd: Option<String>,
```

- [ ] **Step 2: Delete `looks_like_task_cmd` and `normalize_submit_args`**

Delete `looks_like_task_cmd` (lines 541-554) and `normalize_submit_args` (lines 556-591).

In `run_submit` (line 602), remove the call to `normalize_submit_args(&mut args);`.

In `run_submit` signature (line 595), change `mut args: SubmitArgs` to `args: SubmitArgs` — the `mut` is no longer needed since nothing mutates `args` after removing `normalize_submit_args`.

- [ ] **Step 3: Update help text examples**

In `TasksArgs` `after_long_help` (line 33), change:
```
\n  <bold>$ ia tasks submit my-item derive</bold>\
```
to:
```
\n  <bold>$ ia tasks submit my-item --cmd derive</bold>\
```

In `TasksCommand::Submit` `after_long_help` (lines 113-123), update all examples:
```
\n  <bold>$ ia tasks submit my-item --cmd derive</bold>\
\n\n  <dim># Submit with a comment</dim>\
\n  <bold>$ ia tasks submit my-item --cmd make_dark --comment \"curation request\"</bold>\
\n\n  <dim># Submit to multiple items</dim>\
\n  <bold>$ ia tasks submit --cmd derive --itemlist items.txt --comment \"re-derive\"</bold>\
\n\n  <dim># Batch submit from a spreadsheet</dim>\
\n  <bold>$ ia tasks submit --spreadsheet jobs.csv</bold>\
\n\n  <dim># Submit from piped identifiers</dim>\
\n  <bold>$ cat ids.txt | ia tasks submit --cmd derive</bold>\n"
```

- [ ] **Step 4: Update `run_submit` error handling**

The custom error at line 610-612:
```rust
None => bail!("task command is required (e.g. derive, make_dark)"),
```
This is now handled by clap's `required_unless_present`. The `match` on `args.cmd` can stay as-is for the `None` case (spreadsheet mode exits earlier at line 605-607, so if we reach line 610 without a cmd, clap should have already rejected it). But as a safety net, keep the bail.

- [ ] **Step 5: Verify compilation**

Run: `cargo check -p ia-cli`
Expected: Compiles (tests will fail because test args need updating — that's Task 6)

- [ ] **Step 6: Commit**

```
git add ia-cli/src/commands/tasks.rs
git commit -m "refactor(tasks): replace cmd positional with --cmd flag

Change tasks submit cmd argument from a positional to an explicit
--cmd flag. This eliminates the ambiguous positional arg ordering,
the looks_like_task_cmd heuristic, and the normalize_submit_args
function. Any task command string now works with --itemlist/--search.

Clap enforces --cmd is required unless --spreadsheet is provided.
```

---

### Task 6: Update tasks tests and docs

**Files:**
- Modify: `ia-cli/tests/tasks.rs`
- Modify: `docs/usage.md`

- [ ] **Step 1: Update `test_tasks_submit_json` (line 187)**

Change args from `"my-item", "derive"` to `"my-item", "--cmd", "derive"`:
```rust
.args([
    "--insecure", "-H", &host,
    "tasks", "submit", "my-item", "--cmd", "derive", "--json",
])
```

- [ ] **Step 2: Update `test_tasks_submit_batch_itemlist` (line 412)**

Change args from `"derive", "--itemlist"` to `"--cmd", "derive", "--itemlist"`:
```rust
.args([
    "--insecure", "-H", &host,
    "tasks", "submit", "--cmd", "derive",
    "--itemlist", itemlist_path.to_str().unwrap(), "--json",
])
```

- [ ] **Step 3: Update `test_tasks_retry_failed_requires_joblog` (line 629)**

Change args from `"my-item", "derive"` to `"my-item", "--cmd", "derive"`:
```rust
.args(["tasks", "submit", "my-item", "--cmd", "derive", "--retry-failed"])
```

- [ ] **Step 4: Update `test_tasks_submit_wait` (line 646)**

Change args:
```rust
.args([
    "--insecure", "-H", &host,
    "tasks", "submit", "my-item", "--cmd", "derive",
    "--wait", "--wait-interval", "1", "--json",
])
```

- [ ] **Step 5: Update `test_tasks_submit_rate_limited_retry` (line 805)**

Change args:
```rust
.args([
    "--insecure", "-H", &host,
    "tasks", "submit", "my-item", "--cmd", "derive", "--json",
])
```

- [ ] **Step 6: Update `test_tasks_submit_malformed_args` (line 943)**

Change args:
```rust
.args([
    "tasks", "submit", "my-item", "--cmd", "derive",
    "--args", "remove_derived",
])
```

- [ ] **Step 7: Update `test_tasks_submit_wait_with_batch_errors` (line 967)**

Change args:
```rust
.args([
    "--insecure", "-H", &host,
    "tasks", "submit", "--cmd", "derive",
    "--itemlist", itemlist_path.to_str().unwrap(), "--wait",
])
```

- [ ] **Step 8: Update `test_tasks_submit_with_args_and_comment` (line 1010)**

Change args:
```rust
.args([
    "--insecure", "-H", &host,
    "tasks", "submit", "my-item", "--cmd", "derive",
    "--args", "remove_derived=*.jpg",
    "--comment", "re-derive without JPEGs", "--json",
])
```

- [ ] **Step 9: Delete `test_tasks_submit_deprecated_old_arg_order` (line 1189)**

Delete the entire test function (lines 1188-1232). No deprecation bridge exists.

- [ ] **Step 10: Update `test_tasks_submit_cmd_required_without_spreadsheet` (line 1345)**

The error message changes from custom "task command is required" to clap's error. Update assertion:
```rust
assert!(
    stderr.contains("--cmd") || stderr.contains("required"),
    "should explain --cmd is required, got: {stderr}"
);
```

- [ ] **Step 11: Add new regression test for custom command + batch source**

```rust
#[tokio::test]
async fn test_tasks_submit_custom_cmd_with_itemlist() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/services/tasks.php"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "value": { "task_id": 1234, "log": "https://catalogd.archive.org/log/1234" }
        })))
        .expect(2)
        .mount(&mock_server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let itemlist_path = dir.path().join("items.txt");
    std::fs::write(&itemlist_path, "item-one\nitem-two\n").unwrap();

    let host = mock_server.uri().replace("http://", "");
    let output = ia_cmd()
        .args([
            "--insecure", "-H", &host,
            "tasks", "submit",
            "--cmd", "reduce_item",
            "--itemlist", itemlist_path.to_str().unwrap(),
            "--json",
        ])
        .env("IA_ACCESS_KEY_ID", "test_access")
        .env("IA_SECRET_ACCESS_KEY", "test_secret")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "custom command with --itemlist should work, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let success_count = stdout.lines().filter(|l| l.contains("\"success\":true")).count();
    assert_eq!(success_count, 2, "expected 2 successes, got stdout: {stdout}");
}
```

- [ ] **Step 12: Update `docs/usage.md`**

In the tasks submit section (lines 468-515):

Change usage line (line 473) from:
```
ia tasks submit [IDENTIFIER] <CMD> [OPTIONS]
```
to:
```
ia tasks submit [IDENTIFIER] --cmd <CMD> [OPTIONS]
```

Update flags table (lines 478-479): remove `<CMD>` positional row, add `--cmd` as a flag:
```
| `--cmd <CMD>` | Task command (e.g. `derive`, auto-appends `.php` if needed). Required unless `--spreadsheet` is used. |
```

Update all examples (lines 494-515):
```sh
# Submit a derive task
ia tasks submit my-item --cmd derive

# Submit with a comment
ia tasks submit my-item --cmd make_dark --comment "curation request"

# Submit to multiple items from a file
ia tasks submit --cmd derive --itemlist items.txt --comment "re-derive"

# Submit to items from a search query
ia tasks submit --cmd derive --search "collection:nasa" --comment "re-derive all"

# Submit with custom args
ia tasks submit my-item --cmd derive --args remove_derived="*.jpg"

# Submit and wait for completion
ia tasks submit my-item --cmd derive --wait

# Batch submit from a spreadsheet
ia tasks submit --spreadsheet jobs.csv
```

- [ ] **Step 13: Run all tests**

Run: `just ci`
Expected: fmt-check + check + test + doc all pass

- [ ] **Step 14: Commit**

```
git add ia-cli/tests/tasks.rs docs/usage.md
git commit -m "test+docs: update tasks submit tests and docs for --cmd flag

Update all 10 submit tests to use --cmd flag syntax. Delete the
deprecated arg order test (no deprecation bridge needed — positional
cmd was never released). Add regression test for custom command names
with --itemlist. Update docs/usage.md examples.
```

---

### Task 7: Final verification

- [ ] **Step 1: Run `just ci`**

Run: `just ci`
Expected: fmt-check + check + test + doc all pass with 0 failures

- [ ] **Step 2: Verify test count**

Run: `cargo test 2>&1 | tail -5`
Expected: test count >= 1,126 (moved tests + new tests, minus 1 deleted test)

- [ ] **Step 3: Verify doc comments compile**

Run: `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
Expected: Clean build, no warnings (especially check the doc-test path `use ia_core::identifier::validate_identifier` compiles)

- [ ] **Step 4: Push**

Run: `git push`
