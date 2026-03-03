# `ia config` Command Design

**Date**: 2026-03-03
**Status**: Approved

## Overview

Add an `ia config` command with subcommands for authentication setup, config viewing, and credential utilities. This is the first command to use the sub-subcommand pattern, which will be adopted by other commands in future work.

The command produces configs interchangeable with the Python `internetarchive` library — same INI format, same sections, same file paths.

## Command Interface

```
ia config login                    # Interactive login (prompt email + password)
ia config login -u EMAIL -p PASS   # Non-interactive login
ia config login --netrc            # Login using ~/.netrc
ia config show                     # Print current config (secrets redacted)
ia config check                    # Validate S3 keys against IA API
ia config whoami                   # Show account info
ia config print-cookies            # Print cookies (Netscape format)
ia config print-auth               # Print Authorization: LOW header
```

Bare `ia config` (no subcommand) shows help listing available subcommands.

All subcommands support `--json` for structured output and the global `-c`/`--config-file` option.

## Subcommands

### `login`

Authenticates with archive.org and writes credentials to ia.ini.

**Flags:**

| Flag | Short | Type | Description |
|------|-------|------|-------------|
| `--username` | `-u` | `String` | Email address |
| `--password` | `-p` | `String` | Password |
| `--netrc` | `-n` | `bool` | Read credentials from ~/.netrc |

**Credential resolution priority:**
1. `--netrc` → parse `~/.netrc` for `archive.org` entry
2. `-u`/`-p` flags → use directly
3. Interactive → prompt for email (stdin), then password (hidden via `rpassword`)

**Flow:**
1. Resolve credentials
2. POST to `https://{host}/services/xauthn/?op=login` with email + password (form data)
3. Parse response — extract S3 keys, cookies, screenname
4. Merge into existing config file (preserve other sections)
5. Set file permissions to `0o600`
6. Print config file path on success

**API response format:**
```json
{
    "success": true,
    "values": {
        "cookies": {"logged-in-sig": "...", "logged-in-user": "..."},
        "s3": {"access": "...", "secret": "..."},
        "screenname": "...",
        "itemname": "@username"
    }
}
```

**Error codes from API:**
- `account_not_found` → "Account not found, check your email"
- `account_bad_password` → "Incorrect password"
- Generic failure → wrap API message

**Human output:** Config file path on success, colored error on failure.
**JSON output:** `{"config_file": "/path/to/ia.ini"}` on success, `{"error": {...}}` on failure.

### `show`

Displays current config as pretty-printed JSON.

- Loads config from file (respects `-c` global option)
- Redacts secrets: S3 keys and cookies display as `"REDACTED"`
- Human output: colorized pretty-printed JSON
- JSON output: same structure, machine-formatted

### `check`

Validates stored S3 keys against the IA API.

- Loads S3 keys from config
- Calls IA API to verify validity
- Exit code: 0 if valid, 1 if invalid
- Human output: green checkmark + screenname on success, red X on failure
- JSON output: `{"valid": true/false, "screenname": "...", "email": "..."}`

### `whoami`

Retrieves account info from archive.org.

- Loads credentials from config
- Queries IA API for account details
- Human output: key-value display (screenname, email, itemname)
- JSON output: `{"screenname": "...", "email": "...", "itemname": "@..."}`

### `print-cookies`

Outputs cookies in Netscape cookie format (for curl, wget, scripts).

- Loads cookies from config
- Human output: raw Netscape format (designed for piping)
- JSON output: `{"logged-in-user": "...", "logged-in-sig": "..."}`

### `print-auth`

Outputs the Authorization header value.

- Loads S3 keys from config
- Human output: `Authorization: LOW {access}:{secret}` (designed for piping)
- JSON output: `{"header": "Authorization: LOW ..."}`

## Config File Format

INI format, fully interchangeable with Python `internetarchive`:

```ini
[s3]
access = MyAccessKey
secret = MySecretKey

[cookies]
logged-in-user = myemail%40example.com
logged-in-sig = SomeSigCookie

[general]
screenname = myusername
secure = True
host = archive.org
user_agent_suffix = MyApp/1.0

[logging]
level = INFO
file = internetarchive.log
log_to_stdout = False
```

### File path resolution (read — unchanged from current behavior)

1. Explicit path via `-c`/`--config-file` CLI flag
2. `IA_CONFIG_FILE` environment variable
3. `$XDG_CONFIG_HOME/internetarchive/ia.ini`
4. `~/.config/internetarchive/ia.ini`
5. `~/.config/ia.ini` (legacy)
6. `~/.ia` (legacy)

### File path resolution (write — new)

1. If `-c`/`--config-file` was provided, use that path
2. If existing config file found (same search order), update in place
3. If no file exists, create at `$XDG_CONFIG_HOME/internetarchive/ia.ini`

### Write behavior

- Merges new auth values into existing file (preserves custom sections/keys)
- Creates parent directories with mode `0o700` if needed
- Sets file permissions to `0o600` (owner read/write only)

### Config loading priority (unchanged)

```
CLI flags > Environment variables > Config file > Defaults
```

Environment variables:
- `IA_CONFIG_FILE` — config file path
- `IA_ACCESS_KEY_ID` + `IA_SECRET_ACCESS_KEY` — S3 credentials override

## Module Structure

### ia-core (library)

**`auth.rs` (NEW):**
- `login(client, email, password, host) → Result<AuthConfig>` — POST to xauthn, parse response
- `check_keys(client, access, secret) → Result<AccountInfo>` — validate S3 keys
- `whoami(client, access, secret) → Result<AccountInfo>` — get account info
- `AuthConfig` struct: s3 keys, cookies, screenname
- `AccountInfo` struct: screenname, email, itemname
- Error types integrated into `IaError`

**`config.rs` (MODIFIED):**
- `write_config_file(auth_config, path) → Result<PathBuf>` — merge + write INI
- `find_or_default_config_path() → PathBuf` — resolve write target
- `config_to_json(config, redact: bool) → serde_json::Value` — for `show` subcommand

**`lib.rs` (MODIFIED):**
- Expose `pub mod auth`

### ia-cli (binary)

**`commands/config.rs` (NEW):**
- `ConfigCommand` enum with subcommands: `Login`, `Show`, `Check`, `Whoami`, `PrintCookies`, `PrintAuth`
- Each subcommand struct with its flags
- `run()` method dispatching to the appropriate handler

**`main.rs` (MODIFIED):**
- Add `Config(ConfigCommand)` variant to `Commands` enum

## New Dependencies

- `rpassword` — hidden password input for interactive login. Standard, pure Rust, widely used.

## Python Quirks Not Replicated

- 2-second `time.sleep()` after auth request (hardcoded rate-limit workaround)
- `secure` boolean stored as Python string representation (`"True"`) — we'll use standard INI boolean

## Testing Strategy

All tests use wiremock mocks for API calls and tempfile for config file operations.

- **Login success**: mock xauthn → verify config file written with correct values
- **Login errors**: mock xauthn error responses → verify error messages
- **Config merge**: write to existing config → verify other sections preserved
- **File permissions**: verify 0o600 after write
- **Show**: load config → verify JSON output, secrets redacted
- **Check/whoami**: mock API → verify output format
- **Netrc**: test fixture .netrc → verify credential extraction
- **CLI integration**: assert_cmd tests for each subcommand

## Safety Rules Update

This design requires updating the safety rules in CLAUDE.md and MEMORY.md:

**Old rule 1:** "NEVER Write to archive.org During Development/Testing"
**New rule 1:** Same (no change — write operations to IA still require mocks)

**Old rule 2:** "NEVER Use Authentication with archive.org"
**New rule 2:** "Authentication is allowed for read operations and testing. NEVER send write requests (POST/PUT/DELETE/PATCH that modify data) to live archive.org from automated tests. All write operation tests MUST use wiremock mocks. The developer will perform manual live testing."

This allows:
- Reading credentials from config files
- Loading environment variable credentials
- Sending auth headers for read operations
- Testing the login flow against mocked endpoints

This prohibits:
- Sending authenticated write requests to live archive.org from tests
- Metadata modify, upload, delete against live endpoints
