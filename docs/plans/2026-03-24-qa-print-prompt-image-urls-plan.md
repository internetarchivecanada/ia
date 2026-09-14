# `--print-prompt` and `--image-urls` Implementation Plan

**Goal:** Add `--print-prompt` and `--image-urls` flags to `ia ai qa`, plus an improved QA system prompt.

**Architecture:** Three independent changes to ia-core (new `MessageContent::image_url()` constructor, `PageImages` enum in `qa.rs`, `page_image_url()` in `ai/zip.rs`) plus CLI wiring in `commands/ai.rs`. The improved system prompt is a constant replacement in `qa.rs`.

**Tech Stack:** Rust, clap, serde_json, wiremock (tests), assert_cmd (CLI tests)

**Spec:** `docs/plans/2026-03-24-qa-print-prompt-image-urls-design.md`

---

### Task 1: Add `MessageContent::image_url()` constructor

**Files:**
- Modify: `ia-core/src/ai/client.rs:31-44` (add constructor to `impl MessageContent`)

- [ ] **Step 1: Write the failing test**

Add to the existing `#[cfg(test)] mod tests` in `ia-core/src/ai/client.rs`:

```rust
#[test]
fn message_content_image_url_produces_plain_url() {
    let content = MessageContent::image_url("https://example.com/image.jpg");
    let json = serde_json::to_value(&content).unwrap();
    assert_eq!(json["type"], "image_url");
    assert_eq!(
        json["image_url"]["url"],
        "https://example.com/image.jpg"
    );
    // Must NOT start with "data:"
    assert!(
        !json["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:")
    );
}

#[test]
fn message_content_image_base64_produces_data_uri() {
    let content = MessageContent::image_base64("image/jpeg", "abc123");
    let json = serde_json::to_value(&content).unwrap();
    assert_eq!(json["type"], "image_url");
    assert!(
        json["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/jpeg;base64,")
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ia-core message_content_image_url`
Expected: FAIL — `image_url` method does not exist.

- [ ] **Step 3: Write the implementation**

Add to the `impl MessageContent` block in `ia-core/src/ai/client.rs` (after `image_base64`):

```rust
/// Create an image content part from a plain URL (not base64).
///
/// Reuses the `ImageBase64` variant because the OpenAI wire format is
/// identical: `{"type": "image_url", "image_url": {"url": "..."}}`.
/// The API accepts both `data:` URIs and plain `https://` URLs.
pub fn image_url(url: impl Into<String>) -> Self {
    MessageContent::ImageBase64 {
        image_url: ImageUrlContent { url: url.into() },
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-core message_content_image`
Expected: Both new tests PASS. All existing tests still pass.

- [ ] **Step 5: Commit**

```bash
git add ia-core/src/ai/client.rs
git commit -m "feat(ai): add MessageContent::image_url() for plain URL images

Reuses the ImageBase64 variant since the OpenAI wire format is identical
for both data URIs and plain URLs. This enables --image-urls mode where
the LLM provider fetches images directly from archive.org."
```

---

### Task 2: Add `page_image_url()` to `ai/zip.rs`

**Files:**
- Modify: `ia-core/src/download/zip.rs:184` (change `fn` to `pub(crate) fn`)
- Modify: `ia-core/src/ai/zip.rs` (add `page_image_url()` function)

- [ ] **Step 1: Write the failing test**

Add to `#[cfg(test)] mod tests` in `ia-core/src/ai/zip.rs`:

```rust
#[test]
fn page_image_url_constructs_correct_url() {
    let config = crate::config::Config::default();
    let client = crate::IaClient::new_with_config(config).unwrap();
    let url = page_image_url(
        &client,
        "test-item",
        "test-item_jp2.zip",
        "test-item_jp2/page_0001.jp2",
        "jpg",
    );
    assert!(url.contains("test-item"));
    assert!(url.contains("test-item_jp2.zip"));
    assert!(url.contains("page_0001.jp2"));
    assert!(url.ends_with("&ext=jpg"));
    assert!(url.starts_with("https://"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ia-core page_image_url`
Expected: FAIL — function does not exist.

- [ ] **Step 3: Make `build_zip_member_url` pub(crate)**

In `ia-core/src/download/zip.rs:184`, change:

```rust
fn build_zip_member_url(
```

to:

```rust
pub(crate) fn build_zip_member_url(
```

- [ ] **Step 4: Add `page_image_url()` to `ai/zip.rs`**

Add after the `use` statements and before `select_pages`:

```rust
use crate::client::IaClient;

/// Build the download URL for a zip member with format conversion.
///
/// Returns a URL like `https://archive.org/download/{id}/{zip}/{member}&ext={ext}`
/// that triggers server-side conversion (e.g., JP2 → JPEG).
pub fn page_image_url(
    client: &IaClient,
    identifier: &str,
    zip_filename: &str,
    member_path: &str,
    ext: &str,
) -> String {
    let base =
        crate::download::zip::build_zip_member_url(client, identifier, zip_filename, member_path);
    format!("{base}&ext={ext}")
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p ia-core page_image_url`
Expected: PASS.

Run: `cargo test -p ia-core`
Expected: All existing tests still pass.

- [ ] **Step 6: Commit**

```bash
git add ia-core/src/download/zip.rs ia-core/src/ai/zip.rs
git commit -m "feat(ai): add page_image_url() for constructing image URLs

Exposes a public function in ai::zip that builds archive.org download
URLs with format conversion (e.g., JP2 -> JPEG) without downloading.
Used by --print-prompt and --image-urls features."
```

---

### Task 3: Improve QA system prompt + add `PageImages` enum

**Files:**
- Modify: `ia-core/src/ai/qa.rs:106-120` (replace `QA_SYSTEM_PROMPT`)
- Modify: `ia-core/src/ai/qa.rs:142-196` (add `PageImages` enum, update `qa_item` signature)

- [ ] **Step 1: Write tests for the new prompt**

Add to `#[cfg(test)] mod tests` in `ia-core/src/ai/qa.rs`:

```rust
#[test]
fn qa_system_prompt_contains_key_sections() {
    assert!(QA_SYSTEM_PROMPT.contains("two-model consensus"));
    assert!(QA_SYSTEM_PROMPT.contains("Verification Guidelines"));
    assert!(QA_SYSTEM_PROMPT.contains("Titles"));
    assert!(QA_SYSTEM_PROMPT.contains("Dates"));
    assert!(QA_SYSTEM_PROMPT.contains("Authors"));
    assert!(QA_SYSTEM_PROMPT.contains("When to use each verdict"));
    assert!(QA_SYSTEM_PROMPT.contains("Response Format"));
    assert!(QA_SYSTEM_PROMPT.contains("suggested_correction"));
}
```

- [ ] **Step 2: Replace `QA_SYSTEM_PROMPT`**

Replace the constant at `ia-core/src/ai/qa.rs:106-120` with:

```rust
pub const QA_SYSTEM_PROMPT: &str = "\
You are a metadata QA agent performing two-model consensus verification. Another AI \
model extracted metadata from the page images you are about to see. Your job is to \
independently verify each extracted field against the source images.

## Your Task

For each metadata field, examine the page images carefully and determine whether the \
extracted value is correct, incorrect, or uncertain. You are the second pair of eyes — \
be thorough but fair. Minor formatting differences (e.g., \"1988\" vs \"1988-01-01\") are \
acceptable if the core information is correct.

## Verification Guidelines

- **Titles**: Check title pages, cover pages, and headers. Accept minor punctuation or \
capitalization differences if the words match. Flag truncated or substantially different titles.
- **Dates**: Look for dates on title pages, copyright pages, and colophons. A year-only \
extraction is correct if the full date isn't visible. Flag wrong years or decades.
- **Authors/Creators**: Check title pages and bylines. Accept name format variations \
(e.g., \"J. Smith\" vs \"John Smith\") as correct. Flag misspellings or wrong names.
- **Publishers**: Check title pages and copyright pages. Accept abbreviations.
- **Languages**: Verify by examining the actual text content in the images.
- **Subjects/Topics**: Use your judgment — these may not appear verbatim in the images. \
Mark as \"uncertain\" if you cannot verify from visual evidence alone.
- **Schema-defined fields**: If the expected schema defines allowed values or formats, \
verify the extracted value conforms.

## When to use each verdict

- **\"correct\"**: The extracted value accurately represents what is shown in the images. \
High confidence that the extraction is right.
- **\"incorrect\"**: The extracted value clearly contradicts what is shown in the images. \
You MUST provide a \"suggested_correction\" with the correct value.
- **\"uncertain\"**: The field cannot be verified from the available images (e.g., the \
relevant page wasn't included, or the information isn't visually apparent). Do NOT use \
\"uncertain\" as a hedge when you can see the answer — commit to correct/incorrect.

## Response Format

Respond with a JSON object where keys are field names and values are objects with \
\"verdict\", \"confidence\", \"suggested_correction\" (optional), and \"note\" (optional).

{
  \"field_name\": {
    \"verdict\": \"correct\" | \"incorrect\" | \"uncertain\",
    \"confidence\": 0.0 to 1.0,
    \"suggested_correction\": \"...\",
    \"note\": \"...\"
  }
}

Only include fields that were present in the extracted metadata. Do not invent new fields.";
```

- [ ] **Step 3: Run prompt test**

Run: `cargo test -p ia-core qa_system_prompt_contains`
Expected: PASS.

- [ ] **Step 4: Add `PageImages` enum and update `qa_item` signature**

Add the enum before `qa_item()` in `ia-core/src/ai/qa.rs`:

```rust
/// Page images for QA, in either pre-downloaded or URL form.
#[derive(Debug)]
pub enum PageImages {
    /// Downloaded image bytes: Vec of (member_path, jpeg_bytes).
    Base64(Vec<(String, Vec<u8>)>),
    /// Image URLs to pass directly to the LLM: Vec of (member_path, url).
    Urls(Vec<(String, String)>),
}

impl PageImages {
    /// Number of page images.
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            PageImages::Base64(v) => v.len(),
            PageImages::Urls(v) => v.len(),
        }
    }

    /// Whether there are no page images.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
```

Change the `qa_item` signature from:

```rust
pub async fn qa_item(
    _client: &IaClient,
    llm_client: &LlmClient,
    identifier: &str,
    config: &IaAiConfig,
    extracted: &ExtractedMetadata,
    page_images: &[(String, Vec<u8>)],
    opts: &QaOpts,
) -> Result<QaResult> {
```

to:

```rust
pub async fn qa_item(
    _client: &IaClient,
    llm_client: &LlmClient,
    identifier: &str,
    config: &IaAiConfig,
    extracted: &ExtractedMetadata,
    page_images: &PageImages,
    opts: &QaOpts,
) -> Result<QaResult> {
```

Replace the image content building block (lines 159-170, the `let mut content` + `for` loop) with the match below. Keep lines 172-174 (the text prompt push) unchanged after it:

```rust
    // Build vision messages: page images + text prompt
    let mut content: Vec<crate::ai::client::MessageContent> = match page_images {
        PageImages::Base64(images) => images
            .iter()
            .map(|(_filename, image_data)| {
                use base64::Engine;
                let b64 = base64::engine::general_purpose::STANDARD.encode(image_data);
                crate::ai::client::MessageContent::image_base64("image/jpeg", b64)
            })
            .collect(),
        PageImages::Urls(urls) => urls
            .iter()
            .map(|(_member_path, url)| crate::ai::client::MessageContent::image_url(url))
            .collect(),
    };

    // Add the text prompt (unchanged from before)
    let user_text = build_qa_user_message(extracted, config);
    content.push(crate::ai::client::MessageContent::text(user_text));
```

- [ ] **Step 5: Update `dry_run_result` call**

The `dry_run_result` function doesn't use `page_images` so no change needed there. But `qa_item` also needs its internal references updated — the `page_images.len()` calls in the CLI layer need to use `PageImages::len()` (already works via the method added above).

- [ ] **Step 6: Update the existing tests that call `qa_item` or use page_images tuples**

There are no direct tests that call `qa_item` in the unit tests (it's async and needs a real `LlmClient`). The existing tests in `qa.rs` test `parse_qa_response`, `compute_verdict`, `build_qa_user_message`, and serde — none call `qa_item` directly. So no test changes needed here.

However, update the `build_qa_user_message_includes_metadata_and_schema` test to also verify the new prompt:

The test at line 542 already passes because `build_qa_user_message` is unchanged.

- [ ] **Step 7: Run all ia-core tests**

Run: `cargo test -p ia-core`
Expected: All tests PASS.

- [ ] **Step 8: Commit**

```bash
git add ia-core/src/ai/qa.rs
git commit -m "feat(ai): improve QA system prompt and add PageImages enum

Replace minimal QA prompt with comprehensive version that includes
per-field-type verification guidelines (titles, dates, authors, etc.),
clear verdict definitions, and explicit response format instructions.

Add PageImages enum to support both base64-encoded and URL-based image
delivery to the LLM, preparing for --image-urls flag."
```

---

### Task 4: Add CLI flags and `--print-prompt` implementation

**Files:**
- Modify: `ia-cli/src/commands/ai.rs:94-151` (add flags to `QaArgs`)
- Modify: `ia-cli/src/commands/ai.rs:333-482` (update `run_qa` and `run_qa_single`)

- [ ] **Step 1: Write CLI integration tests**

Add to `ia-cli/tests/ai_qa.rs`:

```rust
#[test]
fn ai_qa_help_shows_new_flags() {
    ia().args(["ai", "qa", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--print-prompt"))
        .stdout(predicate::str::contains("--image-urls"));
}

#[test]
fn ai_qa_print_prompt_conflicts_with_promote() {
    let cfg = empty_config();
    ia_with_config(&cfg)
        .args([
            "ai", "qa", "--print-prompt", "--promote", "--api-key", "test",
            "some-item",
        ])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("--print-prompt")
                .and(predicate::str::contains("--promote"))
                .or(predicate::str::contains("cannot be used with")),
        );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-cli --test ai_qa ai_qa_help_shows_new_flags`
Expected: FAIL — flags don't exist in help output yet.

- [ ] **Step 3: Add flags to `QaArgs`**

In `ia-cli/src/commands/ai.rs`, add to the `QaArgs` struct (after `dry_run`):

```rust
    /// Print the prompt that would be sent to the LLM and exit
    #[arg(long)]
    pub print_prompt: bool,

    /// Send image URLs to the LLM instead of downloading and base64-encoding
    #[arg(long)]
    pub image_urls: bool,
```

- [ ] **Step 4: Add validation in `run_qa`**

At the top of `run_qa` (after the stdin check, before collecting identifiers), add:

```rust
    if args.print_prompt && args.promote {
        bail!("--print-prompt cannot be used with --promote (nothing to promote)");
    }
```

- [ ] **Step 5: Add output structs and `QaSingleOpts`**

Add near the top of `ai.rs` (after the imports). Bundle the per-item options into a struct
to avoid clippy `too_many_arguments` (existing function already has 8 params):

```rust
/// Options passed to `run_qa_single` for each item.
struct QaSingleOpts<'a> {
    ai_config_path: Option<&'a std::path::Path>,
    qa_opts: &'a ia_core::ai::qa::QaOpts,
    promote_opts: &'a Option<ia_core::ai::promote::PromoteOpts>,
    quiet: u8,
    json: bool,
    print_prompt: bool,
    image_urls: bool,
}

/// Structured prompt information for `--print-prompt` output.
#[derive(Debug, serde::Serialize)]
struct QaPromptInfo {
    identifier: String,
    model: String,
    temperature: f64,
    max_tokens: u32,
    system_prompt: String,
    user_message: String,
    images: Vec<QaPromptImage>,
}

/// Image entry in `--print-prompt` output.
#[derive(Debug, serde::Serialize)]
struct QaPromptImage {
    url: String,
    member_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
}
```

- [ ] **Step 6: Implement `--print-prompt` path in `run_qa_single`**

Refactor `run_qa_single` to accept the bundled options struct:

```rust
async fn run_qa_single(
    client: &IaClient,
    llm_client: &ia_core::ai::client::LlmClient,
    identifier: &str,
    opts: &QaSingleOpts<'_>,
) -> Result<ia_core::ai::qa::QaResult> {
```

Update all field accesses in the function body from e.g. `qa_opts` to `opts.qa_opts`,
`json` to `opts.json`, `quiet` to `opts.quiet`, etc.

After step 4 (select pages, before the image download loop), insert the `--print-prompt` early exit.
Note: `QA_SYSTEM_PROMPT` was already made `pub const` in Task 3.

```rust
    if opts.print_prompt {
        // Build image URLs without downloading
        let images: Vec<QaPromptImage> = selected_pages
            .iter()
            .enumerate()
            .map(|(i, page_path)| {
                let url = ia_core::ai::zip::page_image_url(
                    client, identifier, &zip_filename, page_path, "jpg",
                );
                let label = if i == 0
                    && config
                        .result
                        .page_info
                        .first()
                        .map(|p| p.page_type == ia_core::ai::ia_config::PageType::Cover)
                        .unwrap_or(false)
                {
                    Some("cover".to_string())
                } else {
                    None
                };
                QaPromptImage {
                    url,
                    member_path: page_path.clone(),
                    label,
                }
            })
            .collect();

        let user_message = ia_core::ai::qa::build_qa_user_message(&extracted, &config);
        let system_prompt = ia_core::ai::qa::QA_SYSTEM_PROMPT;

        if opts.json {
            // Only construct QaPromptInfo for JSON output (avoids Clone requirement)
            let prompt_info = QaPromptInfo {
                identifier: identifier.to_string(),
                model: opts.qa_opts.model.clone(),
                temperature: opts.qa_opts.temperature,
                max_tokens: opts.qa_opts.max_tokens,
                system_prompt: system_prompt.to_string(),
                user_message,
                images,
            };
            println!("{}", serde_json::to_string(&prompt_info)?);
        } else {
            println!("{}", style("System prompt:").bold());
            for line in system_prompt.lines() {
                println!("  {line}");
            }
            println!();
            println!("{}", style("User message:").bold());
            for line in user_message.lines() {
                println!("  {line}");
            }
            println!();
            println!(
                "{} ({} pages):",
                style("Images").bold(),
                images.len()
            );
            for img in &images {
                let label = img
                    .label
                    .as_ref()
                    .map(|l| format!("  ({l})"))
                    .unwrap_or_default();
                println!("  {} {}{}", style("●").cyan(), img.url, label);
            }
            println!();
            println!(
                "Model: {}  Temperature: {}  Max tokens: {}",
                opts.qa_opts.model, opts.qa_opts.temperature, opts.qa_opts.max_tokens
            );
        }

        // Return a placeholder result (not used for output, just satisfies the return type)
        return Ok(ia_core::ai::qa::QaResult {
            identifier: identifier.to_string(),
            overall_confidence: 0.0,
            verdict: ia_core::ai::qa::QaVerdict::NeedsReview,
            extraction_model: extracted.result.ai_request_info.model.clone(),
            qa_model: opts.qa_opts.model.clone(),
            fields: indexmap::IndexMap::new(),
            token_usage: None,
            elapsed_ms: 0,
        });
    }
```

- [ ] **Step 7: Update image download and `qa_item` call for `--image-urls`**

Replace the image download loop and `qa_item` call. Where currently:

```rust
    // Download page images concurrently (convert JP2 -> JPEG)
    let mut page_images = Vec::new();
    for page_path in &selected_pages {
        ...
        page_images.push((page_path.clone(), image_data));
    }
    ...
    let result = ia_core::ai::qa::qa_item(
        client, llm_client, identifier, &config, &extracted, &page_images, qa_opts,
    ).await...
```

Replace with:

```rust
    // Build page images (download or URL-only)
    let page_images = if opts.image_urls {
        let urls: Vec<(String, String)> = selected_pages
            .iter()
            .map(|page_path| {
                let url = ia_core::ai::zip::page_image_url(
                    client, identifier, &zip_filename, page_path, "jpg",
                );
                (page_path.clone(), url)
            })
            .collect();
        ia_core::ai::qa::PageImages::Urls(urls)
    } else {
        let mut images = Vec::new();
        for page_path in &selected_pages {
            let image_data = ia_core::ai::zip::download_zip_member_converted(
                client,
                identifier,
                &zip_filename,
                page_path,
                "jpg",
            )
            .await
            .context(format!("failed to download page image {page_path}"))?;
            images.push((page_path.clone(), image_data));
        }
        ia_core::ai::qa::PageImages::Base64(images)
    };

    if !opts.json && opts.quiet == 0 {
        eprintln!(
            "  {} {} ({} pages, extraction: {})",
            style("●").cyan(),
            identifier,
            page_images.len(),
            extracted.result.ai_request_info.model,
        );
    }

    // 5. Run QA
    let result = ia_core::ai::qa::qa_item(
        client,
        llm_client,
        identifier,
        &config,
        &extracted,
        &page_images,
        opts.qa_opts,
    )
    .await
    .context(format!("QA failed for {identifier}"))?;
```

- [ ] **Step 8: Update the `run_qa_single` call site in `run_qa`**

Build the options struct before the loop, then pass it to each call:

```rust
    let single_opts = QaSingleOpts {
        ai_config_path: ai_config_path.as_deref(),
        qa_opts: &qa_opts,
        promote_opts: &promote_opts,
        quiet,
        json: args.json,
        print_prompt: args.print_prompt,
        image_urls: args.image_urls,
    };
```

Update the call at line 421:

```rust
        match run_qa_single(client, &llm_client, identifier, &single_opts).await
```

- [ ] **Step 9: Suppress summary counts for `--print-prompt`**

In `run_qa`, don't print the pass/fail/review summary when `--print-prompt` is active. Update the verdict counting:

```rust
            Ok(result) => {
                if !args.print_prompt {
                    match result.verdict {
                        ia_core::ai::qa::QaVerdict::Pass => pass_count += 1,
                        ia_core::ai::qa::QaVerdict::Fail => fail_count += 1,
                        ia_core::ai::qa::QaVerdict::NeedsReview => review_count += 1,
                    }
                    if args.json {
                        println!("{}", serde_json::to_string(&result)?);
                    }
                }
                // --print-prompt output is handled inside run_qa_single
            }
```

Also skip the summary block:

```rust
    if quiet == 0 && !args.json && !args.print_prompt {
        // ... existing summary ...
    }
```

- [ ] **Step 10: Update `ai_qa_help_shows_expected_flags` test**

In `ia-cli/tests/ai_qa.rs`, the existing `ai_qa_help_shows_expected_flags` test should also check for the new flags. But we already have a separate test from step 1. No change needed.

- [ ] **Step 11: Run all tests**

Run: `cargo test -p ia-cli --test ai_qa`
Expected: All tests PASS, including the two new ones.

Run: `just ci` (or `cargo fmt --check && cargo check && cargo test && cargo doc`)
Expected: All green.

- [ ] **Step 12: Commit**

```bash
git add ia-cli/src/commands/ai.rs ia-cli/tests/ai_qa.rs ia-core/src/ai/qa.rs
git commit -m "feat(cli): add --print-prompt and --image-urls to ia ai qa

--print-prompt shows the full LLM prompt (system prompt, user message,
image URLs) without calling the API. Useful for debugging and iterating
on AI configs. Supports --json for structured output.

--image-urls sends image URLs directly to the LLM instead of downloading
and base64-encoding. Faster for public items since the LLM provider
fetches directly from archive.org.

Also make QA_SYSTEM_PROMPT pub so --print-prompt can access it."
```

---

### Task 5: Add wiremock integration test for `--image-urls`

This verifies the full CLI flow when `--image-urls` is active sends URL-based image_url content parts to the LLM API.

**Files:**
- Modify: `ia-cli/tests/ai_qa.rs` (add wiremock-based test)

- [ ] **Step 1: Write the integration test**

This test needs wiremock to mock both the archive.org metadata API and the LLM API. Since the existing CLI tests in `ai_qa.rs` don't use wiremock (they only test argument validation), and adding full end-to-end wiremock tests is complex (requires mocking get_item, zip listing, AI config lookup, and LLM endpoint), we should instead add a focused unit test in `ia-core` that verifies the content assembly.

Add to `ia-core/src/ai/qa.rs` tests:

```rust
#[test]
fn page_images_url_mode_produces_url_content() {
    let urls = PageImages::Urls(vec![
        ("page_0000.jp2".to_string(), "https://example.com/page0.jpg".to_string()),
        ("page_0001.jp2".to_string(), "https://example.com/page1.jpg".to_string()),
    ]);

    // Verify we can build MessageContent from URLs
    if let PageImages::Urls(ref url_vec) = urls {
        let content: Vec<crate::ai::client::MessageContent> = url_vec
            .iter()
            .map(|(_, url)| crate::ai::client::MessageContent::image_url(url))
            .collect();

        assert_eq!(content.len(), 2);
        let json = serde_json::to_value(&content[0]).unwrap();
        assert_eq!(json["type"], "image_url");
        assert_eq!(json["image_url"]["url"], "https://example.com/page0.jpg");
        assert!(!json["image_url"]["url"].as_str().unwrap().starts_with("data:"));
    }

    assert_eq!(urls.len(), 2);
    assert!(!urls.is_empty());
}

#[test]
fn page_images_base64_mode_produces_data_uri_content() {
    let images = PageImages::Base64(vec![
        ("page_0000.jp2".to_string(), vec![0xFF, 0xD8, 0xFF]),
    ]);

    if let PageImages::Base64(ref img_vec) = images {
        use base64::Engine;
        let content: Vec<crate::ai::client::MessageContent> = img_vec
            .iter()
            .map(|(_, data)| {
                let b64 = base64::engine::general_purpose::STANDARD.encode(data);
                crate::ai::client::MessageContent::image_base64("image/jpeg", b64)
            })
            .collect();

        assert_eq!(content.len(), 1);
        let json = serde_json::to_value(&content[0]).unwrap();
        assert!(json["image_url"]["url"].as_str().unwrap().starts_with("data:image/jpeg;base64,"));
    }

    assert_eq!(images.len(), 1);
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p ia-core page_images`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add ia-core/src/ai/qa.rs
git commit -m "test(ai): add PageImages unit tests for URL and base64 modes

Verify that PageImages::Urls produces plain URL content parts and
PageImages::Base64 produces data URI content parts."
```

---

### Task 6: Final verification

- [ ] **Step 1: Run full CI**

Run: `just ci`
Expected: fmt-check, check, test, doc all pass.

- [ ] **Step 2: Verify cargo clippy**

Run: `cargo clippy -- -D warnings`
Expected: No warnings.

- [ ] **Step 3: Push**

```bash
git push
```
