# Manual QA Testing — Single Item Mode

The item must have **AI-extracted metadata** (the `extracted_metadata` field in the metadata API) and an **AI Config** associated with its collection.

The example item from the help text is `my-item`.

## Setup

Set your Anthropic API key (fish syntax):

```fish
set -x ANTHROPIC_API_KEY sk-ant-...your-key...
```

The binary path is:
```
/Users/jake/github/jjjake/worktrees/ai-qa/target/debug/ia
```

Alias for convenience:
```fish
alias ia-qa /Users/jake/github/jjjake/worktrees/ai-qa/target/debug/ia
```

## Test Cases (ordered from safest to most involved)

### 1. Help text
Verify the command parses and shows usage:
```fish
ia-qa ai qa --help
```

### 2. No-input error
Should fail with a clear message:
```fish
ia-qa ai qa
```

### 3. Dry run
Fetches metadata + AI config + zip page listing but skips the LLM call. Tests all the plumbing up to the LLM:
```fish
ia-qa ai qa --dry-run --base-url https://api.anthropic.com --api-key $ANTHROPIC_API_KEY --model claude-sonnet-4-6 my-item
```
Expect: per-field results all showing `uncertain` with `confidence: 0.0` and "dry run — not verified" notes.

### 4. Print prompt
Builds the full API request without sending it. Great for inspecting what images and metadata the LLM would see:
```fish
ia-qa ai qa --print-prompt --base-url https://api.anthropic.com --api-key $ANTHROPIC_API_KEY --model claude-sonnet-4-6 my-item
```
Expect: system prompt summary, numbered image URLs with member paths, text block with extracted metadata + schema, and a provider/model/temp summary line.

### 5. Print prompt (JSON)
Machine-readable version of above:
```fish
ia-qa ai qa --print-prompt --json --base-url https://api.anthropic.com --api-key $ANTHROPIC_API_KEY --model claude-sonnet-4-6 my-item
```
Expect: valid JSON with `messages` array, `model`, `temperature`, `max_tokens`.

### 6. Live QA (the real test)
Sends page images + extracted metadata to Anthropic and gets back per-field verdicts:
```fish
ia-qa ai qa --base-url https://api.anthropic.com --api-key $ANTHROPIC_API_KEY --model claude-sonnet-4-6 my-item
```
Expect: progress line with item ID, page count, extraction model; then a verdict line (PASS/FAIL/REVIEW) with confidence % and field count; then a summary.

### 7. Live QA with JSON output
Same but structured:
```fish
ia-qa ai qa --json --base-url https://api.anthropic.com --api-key $ANTHROPIC_API_KEY --model claude-sonnet-4-6 my-item
```
Expect: single JSONL line with `identifier`, `overall_confidence`, `verdict`, `fields` map, `token_usage`.

### 8. Image URLs mode
Passes URLs to the LLM instead of downloading+base64-encoding:
```fish
ia-qa ai qa --image-urls --json --base-url https://api.anthropic.com --api-key $ANTHROPIC_API_KEY --model claude-sonnet-4-6 my-item
```
Note: Anthropic may or may not support image URLs (vs base64). This tests whether the provider handles it or gives a clear error.

### 9. Promote dry run
Shows what fields would be written without writing:
```fish
ia-qa ai qa --promote --dry-run --base-url https://api.anthropic.com --api-key $ANTHROPIC_API_KEY --model claude-sonnet-4-6 my-item
```
Expect: QA results plus "Would promote N field(s), skip M" line. No writes to archive.org.

### 10. Joblog
Verify logging works:
```fish
ia-qa ai qa --joblog /tmp/qa-test.jsonl --base-url https://api.anthropic.com --api-key $ANTHROPIC_API_KEY --model claude-sonnet-4-6 my-item
cat /tmp/qa-test.jsonl
```
Expect: JSONL entry with op `ai-qa`, status `ok`, token usage.

### 11. Resume
Run the same command again with the same joblog and confirm it skips:
```fish
ia-qa ai qa --joblog /tmp/qa-test.jsonl --base-url https://api.anthropic.com --api-key $ANTHROPIC_API_KEY --model claude-sonnet-4-6 my-item
```
Expect: "Resuming: 1 item already QA'd" then "All 1 items already QA'd".

### 12. Missing extracted metadata
Try an item that doesn't have AI extraction:
```fish
ia-qa ai qa --dry-run --base-url https://api.anthropic.com --api-key $ANTHROPIC_API_KEY --model claude-sonnet-4-6 nasa
```
Expect: clear error like "no extracted metadata for nasa".

## What to watch for

- **Error messages**: are they clear and actionable?
- **Provider auto-detection**: does `--base-url https://api.anthropic.com` correctly select the Anthropic provider (check `--print-prompt` output)?
- **Image download**: does it find the JP2 zip and download page images without errors?
- **LLM response parsing**: does the structured JSON parse correctly from Claude's response?
- **Confidence scores**: do they seem reasonable for the content?
- **Token usage**: is it reported in JSON output?

Start with tests 1-5 (no LLM calls, free) and then move to test 6 once those all pass cleanly.
