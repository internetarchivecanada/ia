# Minor Commands Analysis — Batch-Audit Task #6

**Analyst:** minor-commands-analyst
**Date:** 2026-03-16
**Scope:** `collection create`, `config` subcommands, `status`, `update`, `ai`

---

## 1. COLLECTION CREATE

**Files:** ia-cli/src/commands/collection.rs:1–170, ia-core/src/collection.rs:1–609

### Interface

| Aspect | Details |
|--------|---------|
| **Positional args** | `identifier: String` (required) |
| **Named flags** | `-t, --title` (required), `-s, --subject` (required), `-C, --collection` (required), `--description` (required), `-I, --image` (optional Path), `-m, --metadata` (repeatable, `KEY:VALUE`), `--derive` (bool), `--dry-run` (bool), `--json` (bool) |
| **Stdin behavior** | None. Does not read piped input. |
| **Batch input** | None. Single-item only. No `--itemlist`, `--search`, or stdin. |
| **Output format** | Default: colored text (`created: URL` or `dry-run: URL`). `--json`: object with `{identifier, status, url}`. Errors: JSON if `--json`, else propagated. |
| **Global options** | Accepts `quiet: u8` but only uses it to suppress output if `quiet > 0`. No `--joblog`, `--jobs`, `--retry-failed`. |
| **Error handling** | Manual `KEY:VALUE` parsing (anyhow::anyhow!, line 120). Conditional JSON error output (lines 138–147). No joblog integration. |

### Validation & Dry-run

- Core validation via `ia_core::collection::create_collection()` (lines 126–134)
- Validates identifier format (line 46)
- Validates image exists and has extension (lines 48–69)
- Dry-run short-circuits at Core level (lines 194–200, 522–527 in tests)

### Implementation Patterns

- **Collection metadata**: Always prepends `mediatype=collection`, silently drops user-supplied `mediatype` (lines 102–111)
- **Image upload**: Via S3 `{id}_itemimage.{ext}` key (line 144)
- **Without image**: Zero-body PUT to S3 item URL with encoded metadata headers (lines 186–249)
- **Content-Type inference**: Simple extension-based mapping (lines 251–263)

---

## 2. CONFIG COMMAND

**File:** ia-cli/src/commands/config.rs:1–368

### Subcommands

#### 2.1 config login

| Aspect | Details |
|--------|---------|
| **Positional args** | None |
| **Named flags** | `-u, --username` (optional String), `-p, --password` (optional String), `--netrc` (bool), `--json` (bool) |
| **Stdin behavior** | Prompts for email/password if stdin is a terminal; errors if not terminal and flags not provided (lines 221–230, 236–243). Reads from `~/.netrc` if `--netrc` (lines 213–216). |
| **Batch input** | None. Single login. |
| **Output** | Default: eprintln stderr with green checkmark + config file path (lines 265–269). `--json`: `{config_file, screenname}`. |
| **Error handling** | Explicit error if stdin not terminal + flags missing (anyhow::bail!, lines 222–225, 237–240). |

#### 2.2 config show

| Aspect | Details |
|--------|---------|
| **Positional args** | None |
| **Named flags** | `--json` (bool), `--show-secrets` (bool) |
| **Output** | Default: pretty-printed JSON to stdout. `--json`: compact JSON. Secrets redacted by default. |

#### 2.3 config check

| Aspect | Details |
|--------|---------|
| **Positional args** | None |
| **Named flags** | `--json` (bool) |
| **Output** | Default: green checkmark + "Credentials valid (screenname)". `--json`: `{valid, screenname, email, itemname}` or `{valid: false, error}`. |
| **Exit codes** | `exit(1)` if invalid (lines 302–305). |

#### 2.4 config whoami

| Aspect | Details |
|--------|---------|
| **Positional args** | None |
| **Named flags** | `--json` (bool) |
| **Output** | Default: key-value pairs to stdout. `--json`: object. |

#### 2.5 config print-cookies

| Aspect | Details |
|--------|---------|
| **Positional args** | None |
| **Named flags** | `--json` (bool) |
| **Output** | Default: Netscape cookie format (line 342: `.archive.org\tTRUE\t/\tTRUE\t0\t{name}\t{value}`). `--json`: array. |
| **Error** | Errors if no cookies (line 334: `anyhow::bail!`). |

#### 2.6 config print-auth

| Aspect | Details |
|--------|---------|
| **Positional args** | None |
| **Named flags** | `--json` (bool) |
| **Output** | Default: "Authorization: LOW {access}:{secret}". `--json`: `{header}`. |
| **Error** | Errors if no S3 keys (lines 349–354: `anyhow::bail!`). |

### Config Global Options

- No `--joblog`, `--jobs`, `--quiet`, `--retry-failed` support
- `--json` flag handled per-subcommand (mostly consistently)
- No joblog integration

---

## 3. STATUS COMMAND

**File:** ia-cli/src/commands/status.rs:1–242

### Interface

| Aspect | Details |
|--------|---------|
| **Positional args** | None. Requires `--joblog` flag. |
| **Named flags** | `--joblog FILEPATH` (required), `--json` (bool) |
| **Stdin behavior** | None. Requires explicit file path. |
| **Batch input** | None. Single joblog file only. |
| **Output format** | **Default**: Human-readable summary (total, succeeded %, failed %, skipped %) + failed items list + retry hint + AI section if present (lines 109–202). **JSON**: flat object with `total, succeeded, failed, skipped, failures` array (lines 74–87). |
| **Global options** | Only `--json`. No `--joblog`, `--jobs`, `--quiet`, `--retry-failed`. |
| **Error handling** | File existence check (line 33). Context wrapper on read failure (line 36). Exit code 1 if JSON mode and failures (lines 84–86). |

### Special Features

- **Deduplication**: `joblog::summarize_dedup()` (line 56) handles duplicate entries
- **Failed files extraction**: Finds latest error for each `(item, file)` pair (lines 148–171)
- **Retry hints**: Detects operation type (upload vs download) and suggests command (lines 164–170)
  - Download: `ia download --retry-failed --joblog <file>`
  - Upload: `ia upload import <spreadsheet> --retry-failed --joblog <file>`
- **AI summary section** (lines 174–202): Parses AI-specific metrics (items analyzed, changes applied, tokens, undos)

### Joblog Integration

- **No integration with `--retry-failed`**: Suggests command but doesn't use the flag automatically
- **No integration with other commands**: Status is standalone, doesn't trigger retries

---

## 4. UPDATE COMMAND

**File:** ia-cli/src/commands/update.rs:1–344

### Subcommands

#### 4.1 update (bare/default)

| Aspect | Details |
|--------|---------|
| **Positional args** | None |
| **Named flags** | `--check` (bool, check-only), `--json` (bool) |
| **Behavior** | If `--check`, runs `run_check()` (lines 103–105). Otherwise calls `perform_update()` (lines 111–118). |
| **Output** | **Default**: "Checking for updates..." → success message with versions. **JSON**: `{status: "up_to_date" or "updated", current_version, new_version}`. |

#### 4.2 update list

| Aspect | Details |
|--------|---------|
| **Positional args** | None |
| **Named flags** | `--all` (bool, show all vs. 5 recent), `--json` (bool, inherited from parent) |
| **Output** | **Default**: List with installed/platform indicators (lines 239–255). **JSON**: release objects. |

#### 4.3 update install VERSION

| Aspect | Details |
|--------|---------|
| **Positional args** | `version: String` (required) |
| **Named flags** | `--json` (bool, inherited) |
| **Validation** | Rejects versions below `MIN_INSTALLABLE_VERSION` (lines 284–296). |
| **Output** | **Default**: "Installed ia X (was Y)". **JSON**: `{status: "installed", previous_version, installed_version}`. |

### Update Global Options

- No `--joblog`, `--jobs`, `--quiet`, `--retry-failed`
- Parent `--json` cascades to subcommands (lines 86–87, 221, 276)
- Feature-gated: Only available in release builds (line 10–11 comment)

---

## 5. AI COMMAND

**File:** ia-cli/src/commands/ai.rs:1–461

### Subcommands

#### 5.1 ai (bare/default — analysis pipeline)

| Aspect | Details |
|--------|---------|
| **Positional args** | `identifiers: Vec<String>` (optional, variadic) |
| **Input sources** | `--itemlist FILEPATH`, `--search QUERY`, stdin if no args and not terminal (lines 408–422) |
| **Modes** | `--headless` (auto-accept, output JSONL), `--record-only` (TUI + local JSON), `--dry-run` |
| **LLM config** | `--base-url`, `--api-key`, `--model`, `--temperature`, `--max-tokens` |
| **Focus filters** | `--dates-only`, `--titles-only`, `--descriptions-only`, `--missing-fields`, `--schema-fix`, `--typos`, `--only-fields` (comma-delimited), `--exclude-fields` (comma-delimited) |
| **Prompt** | `--prompt-file FILEPATH`, `--system-prompt STRING` |
| **Performance** | `--ai-jobs INT` (default 1, LLM concurrency), `--prefetch INT` (default 5, item prefetch), `--max-tokens-budget INT` |
| **Output** | `-o, --output FILEPATH` (record-only mode), `--json` (output JSONL) |

**Stdin behavior:**
- Lines 408–422: Reads identifiers from stdin if:
  - No identifiers, no `--itemlist`, no `--search`
  - AND stdin is not a terminal
- Supports comments (lines starting with `#`) and blank lines

**Batch input patterns:**
- CLI args (line 66)
- `--itemlist` file (lines 387–396)
- `--search QUERY` with streaming results (lines 398–405)
- stdin piping (lines 408–422)
- Multiple input sources can be combined

**Output format:**
- **Interactive (default)**: TUI dashboard with live progress (lines 316–333)
- **Headless**: JSONL to stdout (one record per item, line 304)
- **Record-only**: TUI + write to JSON file (line 344)
- **Errors**: Pretty-printed to stderr
- **Summary (non-headless)**: Colored stderr summary (lines 237–254, 427–460)

**Global options:**
- Accepts `quiet: u8` and `joblog_path: Option<PathBuf>` in run signature
- Uses joblog_path to write undo information (lines 225–228)
- Respects `quiet` to suppress non-error output (lines 307–314)
- Does **NOT** use: `--jobs` (has `--ai-jobs` instead), `--retry-failed`
- Does integrate with joblog for undo support

**Error handling:**
- Explicit precondition checks (lines 259–270: no identifiers)
- API key validation from multiple sources (lines 276–284)
- Context wrapper on file/stdin reads (lines 388–389, 402, 416)
- Undo error handling (lines 229–235)

**Focus config builder:**
- Lines 179–202: AiArgs::build_focus_config() maps CLI flags to FocusConfig
- Lines 204–213: AiArgs::build_ai_config() merges CLI flags with base config

#### 5.2 ai undo JOBLOG

| Aspect | Details |
|--------|---------|
| **Positional args** | `joblog: PathBuf` (required, path to joblog with changes) |
| **Named flags** | `--dry-run` (bool), `--json` (bool) |
| **Output** | **Default**: Colored stderr summary (items undone, changes reversed, skipped, errored). **JSON**: `{items_undone, changes_reversed, items_skipped, items_errored}`. |
| **Behavior** | Reads joblog, finds successful AI changes, applies reverse operations (lines 224–256) |

### AI Global Options

- `quiet: u8` and `joblog_path: Option<PathBuf>` available
- `--ai-jobs` (parallelism) instead of global `--jobs`
- Joblog integration for undo, not regular batch operations

---

## Cross-Cutting Consistency Issues

### Issue #1: Batch Capabilities Missing in Minor Commands

| Command | Batch Support | Details |
|---------|---------------|---------|
| collection | ✗ | Single item only |
| config | ✗ | Single user/subcommand |
| status | ✗ | Single joblog only |
| update | ✗ | One version at a time |
| ai | ✓ | Full: CLI args, --itemlist, --search, stdin |

**Impact:** Inconsistent UX across commands. Users expect `ia collection create --itemlist collections.txt` to work like upload/download do, but it doesn't exist.

---

### Issue #2: Global Options Not Consistently Used

| Command | --joblog | --jobs | --quiet | --json | --retry-failed |
|---------|----------|--------|---------|--------|----------------|
| collection | ✗ | ✗ | ✗* | ✓ | ✗ |
| config | ✗ | ✗ | ✗ | ✓ | ✗ |
| status | ✗ | ✗ | ✗ | ✓ | ✗ |
| update | ✗ | ✗ | ✗ | ✓ | ✗ |
| ai | ✓ | ✗** | ✓ | ✓ | ✗ |

**Notes:**
- *collection create accepts `quiet` param but only checks if `quiet > 0`, not the global quiet behavior
- **ai uses `--ai-jobs` instead of global `--jobs`

**Impact:** Inconsistent flag surface area. Minor commands don't participate in global parallelism (`--jobs`). Only `ai` uses `--quiet`, status doesn't.

---

### Issue #3: JSON Output Shape Inconsistencies

| Command | JSON Structure |
|---------|-----------------|
| collection create | Simple: `{identifier, status, url}` |
| config show | Nested config structure (all sections) |
| config login | Simple: `{config_file, screenname}` |
| config check | Status: `{valid, screenname, email, itemname}` or `{valid, error}` |
| status | Flat: `{total, succeeded, failed, skipped, failures: [{item, file, error}]}` |
| update | Polymorphic: `{status: "up_to_date"|"updated", versions...}` |
| ai undo | Summary: `{items_undone, changes_reversed, ...}` |

**Impact:** Consumers (scripts, agents) must handle different structures. No consistent "success/error" wrapper.

---

### Issue #4: Error Handling Patterns

| Command | Pattern |
|---------|---------|
| collection | Conditional JSON error (lines 138–147) |
| config | Subcommand-specific patterns (login uses explicit error checks, others use anyhow::bail!) |
| status | Exit code 1 if failures in JSON mode (lines 84–86) |
| update | Consistent: check → if json then write_json_error + exit(1) (lines 162–167) |
| ai | Explicit anyhow::bail! for preconditions, context() for failures |

**Impact:** Unpredictable exit codes and error formats. Some commands exit(1), others propagate with `?`.

---

### Issue #5: Status/Retry Integration Loose

- **status** suggests retry commands (lines 164–170) but doesn't use `--retry-failed` flag
- **upload/download** (analyzed by other team members) support `--retry-failed --joblog <file>`
- **status** does not participate in the retry workflow — it's read-only diagnostics

**Impact:** Users must manually copy-paste retry commands from status output. No automation.

---

### Issue #6: Parallelism Naming Inconsistency

- **Global `--jobs`**: Used by download, upload, tasks (presumably)
- **AI `--ai-jobs`**: LLM request parallelism (default 1)
- **status**: No parallelism support (single joblog processing)

**Impact:** Inconsistent naming. Users expecting `ia ai --jobs 4` will be surprised by `--ai-jobs`.

---

### Issue #7: Missing `--quiet` Support

- Only **ai** command respects `quiet` parameter in run()
- **collection**, **config**, **status**, **update** accept it but ignore it
- **config** subcommands always print to stderr (no quiet mode)

**Impact:** Batch workflows can't silence non-error output from these commands.

---

### Issue #8: Stdin Detection Inconsistency

- **ai** (lines 408–422): Checks `!std::io::stdin().is_terminal()` to detect piped input
- **config login** (lines 221, 236): Same check, errors if not interactive
- **Others**: No stdin support

**Impact:** Confusing UX. `ia ai < identifiers.txt` works, `ia config login < creds.txt` doesn't.

---

### Issue #9: Config Subcommand Pattern Not Reused

- **config** has 6 subcommands (login, show, check, whoami, print-cookies, print-auth)
- **ai** has 2 subcommands (bare, undo)
- **Other minor commands**: No subcommands (except collection which only has create)

**Impact:** Inconsistent command structure. Users might expect `ia collection show <id>` to exist.

---

## Recommendations

### High Priority

1. **Extend collection create to support batch input:**
   - Add `--itemlist FILEPATH` for spreadsheet import (like upload)
   - Reuse metadata parsing logic from upload
   - Write to joblog so status can summarize

2. **Unify parallelism naming:**
   - Rename `ai --ai-jobs` to use global `--jobs` (or document why it's different)
   - Ensure all batch commands respect `--jobs`

3. **Integrate status with `--retry-failed`:**
   - Add `--retry-failed --joblog <file>` support to collection create, config, update
   - Make status output actionable without copy-paste

### Medium Priority

4. **Standardize `--quiet` handling:**
   - All commands should respect quiet parameter
   - Config subcommands should support quiet output

5. **Consistent JSON output shape:**
   - Adopt envelope structure: `{success: bool, data: {...}, error: {...}?}`
   - Or at least document expected shapes per command

6. **Stdin piping consistency:**
   - Decide: should all batch commands support stdin?
   - Document which commands accept piped input

### Low Priority

7. **Consider expanding collection command:**
   - `collection list` to enumerate collections
   - `collection update` to modify metadata
   - (Currently only `create` exists)

---

## Files Reviewed

- `ia-cli/src/commands/collection.rs:1–170`
- `ia-cli/src/commands/config.rs:1–368`
- `ia-cli/src/commands/status.rs:1–242`
- `ia-cli/src/commands/update.rs:1–344`
- `ia-cli/src/commands/ai.rs:1–461`
- `ia-core/src/collection.rs:1–609`

---

## Summary

Minor commands have inconsistent interfaces around batch processing, global options, JSON output, and error handling. Most commands are single-item only (collection, config, status, update), while **ai** has rich batch support. The lack of `--retry-failed` integration and `--quiet` support makes these commands harder to use in scripting workflows.

Recommended approach: standardize on the patterns established by upload/download/tasks, then retrofit collection create and consider expanding other commands to support batch operations.
