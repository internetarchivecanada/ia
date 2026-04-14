# Design: `--print-prompt` and `--image-urls` for `ia ai qa`

**Date**: 2026-03-24
**Branch**: ai-qa
**PR**: #291

## Summary

Two new flags for `ia ai qa`:

1. **`--print-prompt`** — Show the complete prompt that would be sent to the LLM API, then exit. No LLM call, no image download. (Item metadata and zip listing are still fetched from archive.org to construct the prompt.)
2. **`--image-urls`** — Send image URLs directly to the LLM API instead of downloading and base64-encoding. Faster for public items.

Plus an improved QA system prompt with per-field-type verification guidance.

## Motivation

- **Debugging**: Verify the prompt looks right before burning API credits.
- **Iterating on AI configs**: See how different `ia_config.json` schemas/prompts shape the QA request.
- **Auditing**: Inspect exactly what data goes to the model.
- **Performance**: For public items, sending URLs avoids downloading and base64-encoding large JP2 images client-side.

## Feature 1: `--print-prompt`

### CLI

New flag on `QaArgs`:

```rust
/// Print the prompt that would be sent to the LLM and exit
#[arg(long)]
pub print_prompt: bool,
```

Incompatible with `--promote` (nothing to promote — rejected at argument validation).

`--print-prompt` takes precedence over `--dry-run` — the prompt is printed and the command exits. `--dry-run` is silently ignored when combined with `--print-prompt`.

### Behavior

Runs the pipeline up through prompt construction:
1. Fetch item metadata (`get_item`)
2. Extract `extracted_metadata` from item
3. Resolve AI config (local file or collection chain)
4. Find JP2 zip, list zip contents, select pages

Steps 1–4 require HTTP requests to archive.org (item metadata API and zip listing). Then the pipeline **stops** — no image download, no LLM call. Constructs image URLs from the selected page paths and prints the prompt.

Image labels (e.g., "cover") are derived from the `PageType` in the AI config's `pageInfo`. Cover pages are labeled; normal pages are not.

### Human-readable output (default)

```
System prompt:
  You are a metadata QA agent performing two-model consensus verification...

User message:
  ## Extracted Metadata
  {"title": "My Book", ...}

  ## Expected Schema
  {...}

  Please verify each extracted field against the page images shown above.

Images (3 pages):
  ● https://archive.org/download/ID/zip_jp2.zip/item_jp2/item_0000.jp2&ext=jpg  (cover)
  ● https://archive.org/download/ID/zip_jp2.zip/item_jp2/item_0001.jp2&ext=jpg
  ● https://archive.org/download/ID/zip_jp2.zip/item_jp2/item_0002.jp2&ext=jpg

Model: claude-sonnet-4-6  Temperature: 0.2  Max tokens: 4096
```

Multiple items: one block per item separated by blank lines.

### JSON output (`--json --print-prompt`)

```json
{
  "identifier": "my-item",
  "model": "claude-sonnet-4-6",
  "temperature": 0.2,
  "max_tokens": 4096,
  "system_prompt": "You are a metadata QA agent...",
  "user_message": "## Extracted Metadata\n...",
  "images": [
    {
      "url": "https://archive.org/download/my-item/zip_jp2.zip/item_jp2/item_0000.jp2&ext=jpg",
      "member_path": "item_jp2/item_0000.jp2",
      "label": "cover"
    },
    {
      "url": "https://archive.org/download/my-item/zip_jp2.zip/item_jp2/item_0001.jp2&ext=jpg",
      "member_path": "item_jp2/item_0001.jp2"
    }
  ]
}
```

Multiple items: one JSONL line per item.

## Feature 2: `--image-urls`

### CLI

New flag on `QaArgs`:

```rust
/// Send image URLs to the LLM instead of downloading and base64-encoding
#[arg(long)]
pub image_urls: bool,
```

### Behavior

Instead of downloading each page image, converting JP2→JPEG, and base64-encoding:
1. Construct the IA download URL for each selected page (with `&ext=jpg` for server-side conversion)
2. Pass the URL directly as an `image_url` content part to the LLM API

This works because the OpenAI chat completions API (and compatible providers) accept both `data:` URIs and plain `https://` URLs in `image_url` content parts — they use the same wire format.

### Trade-offs

- **Faster**: No client-side download/encode. The LLM provider fetches directly from archive.org.
- **Restricted items**: Will fail if the item is not publicly accessible — the LLM provider will get a 403 or HTML error page. No auto-detection; this is the user's responsibility.

### No auto-detection

We do not check whether an item is public before sending URLs. If `--image-urls` is used on a restricted item, the LLM API will receive an error page instead of an image. Keep it simple.

### Flag interactions

- **`--image-urls --print-prompt`**: Prints the prompt showing image URLs, then exits. The image URLs displayed are the same regardless of `--image-urls` since `--print-prompt` always shows URLs (never base64 data).
- **`--image-urls --dashboard`**: Composes naturally — the dashboard displays the same QA results regardless of image delivery mode.
- **`--image-urls --dry-run`**: Composes naturally — dry-run short-circuits before the LLM call either way.

## Improved QA System Prompt

Replace the current minimal `QA_SYSTEM_PROMPT` with:

```text
You are a metadata QA agent performing two-model consensus verification. Another AI
model extracted metadata from the page images you are about to see. Your job is to
independently verify each extracted field against the source images.

## Your Task

For each metadata field, examine the page images carefully and determine whether the
extracted value is correct, incorrect, or uncertain. You are the second pair of eyes —
be thorough but fair. Minor formatting differences (e.g., "1988" vs "1988-01-01") are
acceptable if the core information is correct.

## Verification Guidelines

- **Titles**: Check title pages, cover pages, and headers. Accept minor punctuation or
  capitalization differences if the words match. Flag truncated or substantially
  different titles.
- **Dates**: Look for dates on title pages, copyright pages, and colophons. A year-only
  extraction is correct if the full date isn't visible. Flag wrong years or decades.
- **Authors/Creators**: Check title pages and bylines. Accept name format variations
  (e.g., "J. Smith" vs "John Smith") as correct. Flag misspellings or wrong names.
- **Publishers**: Check title pages and copyright pages. Accept abbreviations.
- **Languages**: Verify by examining the actual text content in the images.
- **Subjects/Topics**: Use your judgment — these may not appear verbatim in the images.
  Mark as "uncertain" if you cannot verify from visual evidence alone.
- **Schema-defined fields**: If the expected schema defines allowed values or formats,
  verify the extracted value conforms.

## When to use each verdict

- **"correct"**: The extracted value accurately represents what is shown in the images.
  High confidence that the extraction is right.
- **"incorrect"**: The extracted value clearly contradicts what is shown in the images.
  You MUST provide a "suggested_correction" with the correct value.
- **"uncertain"**: The field cannot be verified from the available images (e.g., the
  relevant page wasn't included, or the information isn't visually apparent). Do NOT
  use "uncertain" as a hedge when you can see the answer — commit to correct/incorrect.

## Response Format

Respond with a JSON object where keys are field names and values are objects:

{
  "field_name": {
    "verdict": "correct" | "incorrect" | "uncertain",
    "confidence": 0.0 to 1.0,
    "suggested_correction": "...",  // required when verdict is "incorrect"
    "note": "..."                   // explain reasoning for non-"correct" verdicts
  }
}

Only include fields that were present in the extracted metadata. Do not invent new fields.
```

## Implementation Changes

### ia-core

**`client.rs`** — Add URL-based image constructor:

```rust
impl MessageContent {
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
}
```

The existing `ImageBase64` variant serializes as `{"type": "image_url", "image_url": {"url": "..."}}` which is the correct wire format for both data URIs and plain URLs. No new variant needed.

**`qa.rs`**:
- Replace `QA_SYSTEM_PROMPT` with the improved version above
- Add page images enum to represent both delivery modes:

```rust
/// Page images for QA, in either pre-downloaded or URL form.
pub enum PageImages {
    /// Downloaded image bytes: Vec of (member_path, jpeg_bytes).
    Base64(Vec<(String, Vec<u8>)>),
    /// Image URLs to pass directly to the LLM: Vec of (member_path, url).
    Urls(Vec<(String, String)>),
}
```

- Change `qa_item()` signature from `page_images: &[(String, Vec<u8>)]` to `page_images: &PageImages`. Inside the function, match on the enum to build either base64 or URL `MessageContent` parts:

```rust
let content = match page_images {
    PageImages::Base64(images) => {
        images.iter().map(|(_, data)| {
            let b64 = base64::engine::general_purpose::STANDARD.encode(data);
            MessageContent::image_base64("image/jpeg", b64)
        }).collect()
    }
    PageImages::Urls(urls) => {
        urls.iter().map(|(_, url)| {
            MessageContent::image_url(url)
        }).collect()
    }
};
```

**`ai/zip.rs`** — Add a public function to construct page image URLs without downloading:

```rust
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
    let base = crate::download::zip::build_zip_member_url(client, identifier, zip_filename, member_path);
    format!("{base}&ext={ext}")
}
```

**`download/zip.rs`** — Make `build_zip_member_url` `pub(crate)` so `ai/zip.rs` can call it:

```rust
pub(crate) fn build_zip_member_url(...) -> String { ... }
```

### ia-cli

**`commands/ai.rs`**:

- Add `--print-prompt` and `--image-urls` flags to `QaArgs`
- Validate: `--print-prompt` + `--promote` is an error
- `--print-prompt` path in `run_qa_single`: run steps 1-4 (fetch metadata, config, zip listing, page selection), construct URLs, build the user message text, then return a structured prompt object instead of calling the LLM
- `--image-urls` path: skip image download loop, construct URLs, pass URL-based content to `qa_item()`

### Tests

- **Unit**: `MessageContent::image_url()` produces correct JSON (plain URL, not data URI)
- **Unit**: `MessageContent::image_base64()` produces correct JSON (data URI)
- **Unit**: Improved prompt constant contains key sections (verification guidelines, verdict definitions, response format)
- **Unit**: `qa_item` with `PageImages::Urls` produces URL-based content parts (not data URIs)
- **Unit**: `qa_item` with `PageImages::Base64` produces base64 content parts (existing behavior)
- **Unit**: `page_image_url()` constructs correct archive.org URL with `&ext=jpg`
- **Integration (CLI)**: `--print-prompt` outputs system prompt, user message, and image URLs
- **Integration (CLI)**: `--json --print-prompt` outputs valid JSON with expected fields
- **Integration (CLI)**: `--print-prompt --promote` is rejected with an error message
- **Integration (CLI)**: `--image-urls` sends `image_url` content parts to wiremock LLM endpoint (not data URIs)
