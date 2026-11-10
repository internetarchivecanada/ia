# `ia account`: lookup, lock, unlock, and bulk input (#141)

**The gap.** The Python client has `ia account` (`internetarchive/cli/ia_account.py`, backed by `internetarchive/account.py`): look an archive.org account up by email address, screen name or item name, print one of its fields or whether it is locked, and lock or unlock it with a comment. The API is admin-only. ia-cli has no such command, and moderation work that locks many accounts at once runs the Python command once per account under an external job runner. This plan ports the command, adds bulk input in the style of the other batch commands, and adds an `ia_core::account` module for library users.

No request to archive.org is made while building or testing this. Every test runs against a local wiremock server, with addresses under `example.com` and `example.org` and invented screen names and item names.

## API contract (from the Python client)

Taken from `internetarchive/account.py`, `internetarchive/cli/ia_account.py` and `internetarchive/session.py`. Facts the local source does not settle are listed under "Not known".

| | Lookup | Lock / unlock |
|---|---|---|
| Method and path | `POST /services/xauthn/?op=info` | `POST /services/xauthn/?op=lock_unlock` |
| Form body | one field named after the lookup type: `email=…`, `screenname=…` or `itemname=…` (an item name keeps its leading `@`) | `itemname=<item name from the lookup>`, `is_lock=1` (lock) or `is_lock=0` (unlock), and `comments=<text>` only when the comment is non-empty |
| Credentials | the `logged-in-user` and `logged-in-sig` cookies from the `[cookies]` config section, scoped to `.archive.org`; no `Authorization` header | same |
| Success | HTTP 2xx, JSON `{"success": true, "values": {…}}` | HTTP 2xx; the Python client never parses the body and prints it as text |
| Failure | non-2xx; or a 2xx JSON body whose `error` is truthy or whose `values` is missing or empty, for example `{"success": false, "error": "account not found", "values": {}}` | non-2xx |

Account fields in `values`, as the Python `Account` class reads them: `canonical_email`, `email`, `has_disability_access` (bool), `itemname`, `locked` (bool), `notifications` (list of strings), `screenname`, `verified` (bool), `lastlogin`, `createdate`.

The Python CLI always looks the account up first, then sends the lock or unlock request with the item name from the lookup: two requests per account. It does not check the current lock state and does not look the account up again afterwards.

**Lookup type** (`ia_account.py`): a value that starts with `@` is an item name; otherwise a value that matches `^[a-zA-Z0-9_.+-]+@[a-zA-Z0-9-]+\.[a-zA-Z]{2,}$` is an email address; anything else is a screen name.

**Related code in this repository.** `ia_core::auth::check_keys` already posts `op=info` to the same endpoint, with no lookup field and `Authorization: LOW`, and reads back the caller's own account (`auth::AccountInfo`). So the endpoint answers `op=info` without a lookup field with the caller's own record. The new module is separate because it acts on other people's accounts and authenticates as the Python client does.

**Not known** (only a live request could tell; none is made):
1. The body of a lock or unlock answer, and whether a refused lock answers 2xx.
2. What a non-admin caller or an unknown account receives. The Python test fixture uses the `account not found` body above; the Python changelog says some requests are answered with HTTP 400 and an HTML page.
3. Whether `Authorization: LOW` alone is accepted for lookups of other accounts and for `lock_unlock`. Not needed: the port sends what the Python client sends.
4. Whether locking an account that is already locked is an error, and whether each lock records its comment again.
5. Whether the server's `email` or `canonical_email` can differ from the address that found the account in more than letter case (see "Identity check").

## Module layout

- `ia-core/src/account.rs`, declared in `ia-core/src/lib.rs`.
  - `pub enum LookupType { Email, Screenname, Itemname }`: `LookupType::detect(&str)`, `field(self) -> &'static str` (the form field name), `Display` (`email`, `screenname`, `itemname`).
  - `pub fn is_email(&str) -> bool`.
  - `pub struct Account { pub lookup: LookupType, pub values: serde_json::Map<String, Value> }` with accessors `email()`, `canonical_email()`, `screenname()`, `itemname()` returning `Option<&str>` and `locked()` returning `Option<bool>`. Only the field an action uses is required, and only by that action, so an account whose `canonical_email` is null can still be locked. `values` is the whole object as received, so fields the port does not read still reach `--json` output.
  - `pub async fn lookup(client, user: &str) -> Result<Account>` (type detected) and `pub async fn lookup_by(client, LookupType, value: &str) -> Result<Account>`.
  - `pub async fn set_locked(client, itemname: &str, locked: bool, comment: Option<&str>) -> Result<serde_json::Value>`: the answer parsed as JSON, or a JSON string holding the body when it is not JSON. `lock` and `unlock` are thin wrappers.
  - `pub fn cookie_header(client: &IaClient) -> Result<String>`: the header value, after the host check below.
- `ia-core/src/error.rs`: one new variant `IaError::AccountApi { account: String, message: String }`, displayed as `account API error for {account}: {message}`, JSON code `account_api_error` with `account` in the extra fields (as other variants add `identifier`), not retryable. Added to the enum, both doctest variant lists, `is_retryable` and `to_json_error`.
- `ia-cli/src/commands/account.rs`, `Commands::Account` with visible alias `ac` (the Python alias), the module line in `commands/mod.rs`, and the dispatch arm in `lib.rs`, which passes `cli.jobs` as the `Option` (as `metadata` does) so the command applies its own default.

## Requests

**Host.** `client.url("/services/xauthn/")`, so `-H` and `--insecure` apply as for every other command. (The Python CLI ignores `-H` and `-c` for the lookup and always uses `https://`; that is not copied.)

**Credentials.** A `Cookie: logged-in-user=<v>; logged-in-sig=<v>` header built from `[cookies]`, each value sent as stored (`%40` stays encoded), cut at its first `;` and trimmed. The config loader already cuts at `;`; the cut is repeated in `cookie_header` for an `IaConfig` built in code. No `Authorization` header is sent: the Python client sends none on these calls.
- When either cookie is missing or empty: `IaError::Auth("ia account needs the logged-in-user and logged-in-sig cookies; run `ia config login`")`, before any request.
- **Cookie host check.** The Python client scopes these cookies to `.archive.org`, so they never reach another host. The port sends them only when the configured host is `archive.org`, ends in `.archive.org`, or is a loopback address (`localhost`, `127.0.0.1`, `::1`, with any port, for tests and local servers). Any other host is `IaError::Auth("ia account sends login cookies only to archive.org; host is <host>")` before any request. (S3 keys go to any `-H` host today; the admin session cookie is kept to the scope the Python client gives it.)
- The CLI runs this check once before the first account, so a bulk run fails at once rather than once per line.

**Transport: `client.no_redirect_http()` for both requests.** The API transport follows redirects to `*.archive.org`, and reqwest drops the `Cookie` header on a redirect that changes host, port or scheme and turns a POST answered with 301, 302 or 303 into a body-less GET. A followed redirect would therefore reach the server with no credentials and no lookup field, and its answer would be misleading. A 3xx answer is `IaError::Http { status, message: "redirected to <Location>; not followed" }`. This transport has no retry layer, as the Python client has none; a 5xx on the lock is never replayed (the server may already have acted), and a 5xx on the lookup fails that account.

**Lookup response handling, in order.**
1. 429: `IaError::RateLimited { retry_after }`, Retry-After seconds or 30 when absent (the default `metadata/write.rs` uses).
2. 3xx: as above. Other non-2xx: `IaError::Http { status, message: <body>, retry_after }`, the whole body as the message, as other `Http` errors in ia-core.
3. Body not JSON: `AccountApi { message: "response is not JSON: <body>" }`.
4. `success` is `false`, or `error` is truthy, or `values` is missing, not an object, or empty: `AccountApi { message: <error text, or "account not found" when there is none> }`. (The Python client ignores `success`; a `false` there is treated as a failure so the lookup and the lock check the same shape.)
5. Identity check (below).

**Identity check.** `op=info` without a lookup field answers with the caller's own account. If the server ever ignored the field, a lock would land on the operator's account. So every lookup checks that the answer is the account asked for, ignoring letter case: an email lookup matches `email` or `canonical_email`; a screen-name lookup matches `screenname`; an item-name lookup matches `itemname`. A mismatch, or a missing field to compare, is `AccountApi { message: "lookup answered with a different account" }`. The Python client has no such check. If the server matches addresses more loosely than by case (not known, item 5), such a lookup fails with that message and the `@`item name reaches the account.

**Lock and unlock.** The item name must start with `@` and have at least one more character, else `AccountApi { message: "lookup returned no usable itemname" }` and no lock request. 429, 3xx and other non-2xx as above. On 2xx the body is parsed as JSON when it is JSON; an object with `"success": false` or a truthy `error` is `AccountApi` with the error text (or `lock refused` / `unlock refused`). Any other 2xx answer, JSON or not, is success, as in the Python client.

`account` in `AccountApi` is the value the caller passed (the input line), so a bulk error names the line it came from.

## Lookup type

`LookupType::detect` follows the Python CLI's order: a leading `@` is an item name; otherwise an email address; otherwise a screen name. Confirmed against `ia_account.py`.

`is_email` keeps the Python rule's character sets but accepts a domain of two or more dot-separated labels: a local part of one or more of `A-Z a-z 0-9 _ . + -`, one `@`, then labels of one or more of `A-Z a-z 0-9 -`, separated by single dots, the last label two or more ASCII letters. The Python pattern allows exactly two domain labels, so it sends an address on a subdomain or a two-part country domain to the screen-name lookup, which cannot find it. No regex crate is added; the check is plain `str` code.

There is no flag to force a lookup type, as in the Python CLI. A screen name shaped like an email address is looked up as an email address; its `@`item name reaches the same account.

## CLI surface

```
ia account [OPTIONS] [USER]
ia account [OPTIONS] --itemlist <FILE>
… | ia account [OPTIONS]
```

| Flag | Python | Meaning |
|---|---|---|
| `USER` | positional `user` | email address, screen name or `@`item name |
| `-g, --get-email` | `-g` | print the canonical email address (the Python flag prints `canonical_email`, not `email`) |
| `-s, --get-screenname` | `-s` | print the screen name |
| `--get-itemname` | `-i` | print the item name |
| `--is-locked` | `-l` | print `true` or `false` |
| `-L, --lock` | `-L` | lock |
| `-u, --unlock` | `-u` | unlock |
| `--comment <TEXT>` | `-c` | comment recorded with the lock or unlock; an empty comment is not sent, as in the Python client |
| `--itemlist <FILE>` | — | one account per line; `-` reads standard input |
| `--dry-run` | — | print what would be done, send nothing |
| `--json` | — | one JSON object per account on stdout |

The global flags `-j/--jobs`, `--joblog`, `--no-resume`, `-q`, `-H`, `--insecure` and `-c/--config-file` apply.

**Short flags.** `-i`, `-l` and `-c` are global flags in ia-cli (`--insecure`, `--log`, `--config-file`; AGENTS.md reserves them, and clap's debug check rejects a subcommand short flag that repeats a global one), so `--get-itemname`, `--is-locked` and `--comment` are long-only. The other Python short flags are kept.

**Exclusions.** The six action flags are one clap group, at most one at a time; with none, the command prints the whole account. `--comment` requires `--lock` or `--unlock` (the Python CLI ignores a comment given with a read; here that is a usage error, so a curate comment is never dropped unnoticed). `USER` conflicts with `--itemlist`.

**Input sources**, in the order `collect_identifiers` in `ia-cli/src/identifier.rs` uses: the positional `USER`; else `--itemlist <FILE>`; else standard input when it is not a terminal. `--itemlist -` reads standard input (the issue asks for it; no other command has it yet, where `--itemlist /dev/stdin` is the documented form, which also works here); with a terminal on standard input it is the error `--itemlist - needs standard input from a pipe or file`. With none of these sources: `no account given: pass USER, --itemlist <FILE>, or pipe accounts on standard input`. An empty list: `no accounts found in <source>`. As with other commands, a job runner that leaves standard input open as a pipe makes a command with no `USER` wait for it.

Each line is trimmed; blank lines and lines starting with `#` are skipped; repeated lines are dropped, the first kept. Lines are not validated as identifiers (`validate_identifier` would reject email addresses with `+` and screen names with spaces, so `collect_identifiers` itself is not reused) and not parsed as JSON.

## Running

Every form runs the same per-account code: a positional `USER` is a list of one. Accounts run concurrently up to `--jobs`, default 2, the same default as `metadata modify` (the other command that writes through an admin-facing API); no maximum, as for other commands. Results print in completion order.

Per account: look up; for lock and unlock, then send the request with the looked-up item name. Shared `RateLimiter`: on `IaError::RateLimited` the first worker prints `Rate limited. Pausing all workers for {secs}s...` to stderr, every worker waits, and the request that got the 429 is sent again, with no cap, as in `metadata modify`. A 429 on the lock request repeats only the lock request. A lock is sent whatever the account's current state, as in the Python client.

**Several lines for one account.** `Person@Example.com`, `person@example.com`, `example-user` and `@example-user` can all name one account. For lock and unlock, requests are de-duplicated within a run by the looked-up item name (ignoring case): the first line to reach the lock step sends it, later lines for the same item name wait for that result and report it as their own, with `same account as <first line>`. One account gets one lock request and one curate comment per run. Read actions are not de-duplicated; a second lookup is harmless.

## Output

Follows ia-cli's other commands, not the Python output byte for byte (the issue's scope). Differences from the Python CLI: `true`/`false` rather than `True`/`False`; the whole account is the `values` object, indented, rather than ten fields on one line; a lock prints a result line, and the server's answer is in `--json` output; errors go to stderr as `error: …`.

**Human, positional `USER`** (stdout):
- no action flag: the `values` object as indented JSON;
- `--get-email`, `--get-screenname`, `--get-itemname`: the value;
- `--is-locked`: `true` or `false`;
- `--lock` / `--unlock`: `<USER>: locked` / `<USER>: unlocked`.

**Human, list input** (stdout, each line prefixed by its input line so results match input): `<line>: <values as one-line JSON>`, `<line>: <value>`, `<line>: true`, `<line>: locked`, `<line>: locked (same account as <other line>)`.

**Quiet levels**, as in `metadata modify`: read values always print (they are the output); lock and unlock result lines are dropped at `-q`; `error: <line>: <message>` lines (stderr, the error chain as `format_error_chain` prints it) are dropped at `-qq`. After a list run with more than one account, unless `--json` or `-qq`: `<n> succeeded, <m> failed` on stderr. Exit status 1 through `std::process::exit(1)` when any account failed (no extra `Error:` line), else 0.

**`--json`** (stdout, one compact object per account, both forms):

```json
{"account":"person@example.com","lookup":"email","action":"get-email","status":"ok","canonical_email":"person@example.com","elapsed_ms":12}
{"account":"@example-user","lookup":"itemname","action":"lookup","status":"ok","values":{"…":"…"},"elapsed_ms":9}
{"account":"example-user-1","lookup":"screenname","action":"is-locked","status":"ok","locked":false,"elapsed_ms":8}
{"account":"example-user-1","lookup":"screenname","action":"lock","status":"ok","itemname":"@example-user-1","response":{"…":"…"},"elapsed_ms":20}
{"account":"@example-user-1","lookup":"itemname","action":"lock","status":"ok","itemname":"@example-user-1","same_as":"example-user-1","elapsed_ms":20}
{"account":"nobody@example.org","lookup":"email","action":"lock","status":"error","error":{"code":"account_api_error","message":"account API error for nobody@example.org: account not found","account":"nobody@example.org"},"elapsed_ms":7}
```

`action` is one of `lookup`, `get-email`, `get-screenname`, `get-itemname`, `is-locked`, `lock`, `unlock`. The key is `account` rather than the `item` other commands use, because the value is an account, which may be an email address or a screen name, not an item identifier. The `error` object is the body of `IaError::to_json_error` (`{"code","message",…}`, not the outer `{"error": …}` wrapper). Command-level errors (missing cookies, a host the cookies may not go to, an unreadable list, no input) are `{"error": {…}}` on stderr through `write_json_error`, exit 1.

## `--dry-run`

Sends nothing, not even the lookup: the lookup reads a real person's account through an admin API, and `tasks submit --dry-run` likewise sends nothing. The cookie checks are skipped. Allowed with every action, so a list can be checked for how each line will be looked up. Per-account lines go to stdout, as in `metadata modify --dry-run`.

- Human: `dry-run <line>: <action> (lookup by <type>)`, with `, comment "<text>"` added for lock and unlock with a non-empty comment. Then, unless `-qq`, `dry-run: would <action> <n> account(s), nothing sent` on stderr.
- `--json`: `{"account","lookup","action","status":"dry_run","comment"?,"dry_run":true}` per account.
- Not written to the job log. The resume skip set is applied first, so a dry run shows what a resumed run would do.

## Job log

Only lock and unlock use `--joblog`, as only writes resume in other commands (`ia metadata` reads do not). With a read action `--joblog` is ignored, as other commands that do not resume ignore it.

`--joblog <PATH>` records one entry per account as it finishes: `op` `account-lock` or `account-unlock`, `item` the input line, `file` empty, `status` `ok` (bytes 0, `elapsed_ms`) or `error` with the message. A line resolved as the same account as another line gets that line's status.

**Resume.** On a rerun with the same `--joblog` (and no `--no-resume`), a line is skipped when its latest entry among `account-lock` and `account-unlock` is `ok` and has this run's op. Taking the latest of both ops means a lock list rerun after the same accounts were unlocked through the same job log locks them again, and an unlock run is not skipped because of an earlier lock. The skip prints `resuming: skipping <n> account(s) already done in joblog` on stderr unless `-q`; lines that failed run again. When every line is already done the command prints that line and exits 0.

`ia status --joblog <PATH> --failed-items` lists lines that never succeeded. It does not look at `op`, so docs/usage.md advises one job log per action.

**Interruptions.** A Ctrl-C while a lock request is in flight, or a failure after the server may have acted (a 5xx, a read timeout), leaves no `ok` entry, so a rerun sends that lock again with its comment. docs/usage.md says so.

## Errors

| Situation | Error | JSON code |
|---|---|---|
| cookies missing; host not archive.org or loopback | `IaError::Auth` | `auth_error` |
| 429 | `IaError::RateLimited` (pause and retry; not printed per account) | — |
| 3xx; other non-2xx | `IaError::Http` | `http_error` |
| `error` or `success: false` body, empty `values`, non-JSON lookup answer, identity mismatch, missing field for the action, unusable item name, refused lock | `IaError::AccountApi` | `account_api_error` |
| connection failure | `IaError::Network` | `network` |

## Tests (all against wiremock; no request leaves 127.0.0.1)

Test data: addresses under `example.com` and `example.org` only; screen names and item names such as `example-user-1` and `@example-user-1`.

ia-core unit tests in `account.rs` (each builds `IaConfig` in code with test cookies and the mock server's host):
- `is_email` / `LookupType::detect`: `@example-user` → item name; `person@example.com`, `First.Last+tag@mail.example.org`, `a@sub.example.org` → email; `example-user-1`, `person@localhost`, `person@example.c`, `a@b..example.com`, `a@-b.example.com`, `a@example.com.`, `a@example.c0m`, `name with spaces` → screen name.
- Lookup by each type posts to `/services/xauthn/` with `op=info`, the one form field, the `Cookie` header exactly as stored, and no `Authorization` header (checked on `received_requests`); the full `values` map is kept.
- Cookie values with `; path=/; domain=.archive.org` are cut at `;`; a missing or empty cookie, and a host such as `example.org`, are `Auth` with no request (`expect(0)`); `localhost:PORT` and `127.0.0.1:PORT` are allowed.
- `{"success":false,"error":"account not found","values":{}}`, `{"success":true,"values":{}}`, `{"success":false,"values":{…}}`, non-JSON 200 → `AccountApi` naming the input; 400 with HTML → `Http { status: 400 }` with the body; 302 with `Location` → `Http { status: 302 }` and the redirect target not requested; 429 with `Retry-After: 5` → `RateLimited { retry_after: 5 }`, without it → 30; 500 → `Http`, one request.
- Identity: an answer for a different screen name, email or item name → `AccountApi`; matching ignores case; an email lookup matching only `canonical_email` passes.
- Accessors: `canonical_email` null still allows `itemname()`; `locked` not a bool → `None`.
- `set_locked` posts `op=lock_unlock`, `itemname`, `is_lock=1`/`0`, `comments` only when non-empty (`Some("")` sends none); a JSON 2xx answer is returned as JSON, a text 2xx answer as a JSON string; `{"success":false,"error":"…"}` → `AccountApi`; 500 → one request only; an item name without `@` or equal to `@` → `AccountApi` with no request.
- `IaError::AccountApi` → `account_api_error` with `account`, not retryable.

CLI tests in `ia-cli/tests/account.rs`, running the built binary. Each test writes a config with `[general] host = 127.0.0.1:PORT`, `secure = false` and test cookies, passes it with `--config-file`, and removes the `IA_*` environment variables, so a request the test did not expect can only reach the mock, where `expect` counts catch it:
- each action for a positional user, human and `--json`; lock and unlock send a lookup then one lock request (`expect(1)` each), with the comment; `--unlock --comment` accepted; `--comment ""` sends no `comments` field;
- unknown account: exit 1, `error: <user>: …` on stderr, no lock request;
- clap: two action flags, `--comment` without `--lock`/`--unlock`, `USER` with `--itemlist` are usage errors; `-i`, `-l` and `-c` keep their global meaning;
- missing cookies, and a non-archive.org `-H` host: exit 1, no request; with `--json` the error is `{"error":{…}}` on stderr;
- list from a file, from `--itemlist -` and from piped stdin: blank, `#` and repeated lines skipped; each line's lookup type; one result line per account; one failing account gives exit 1 with the others completed and the summary line;
- two spellings of one account in a lock list: one lock request, both lines reported ok, the second with `same account as`;
- `--json` list: one object per account, error objects included;
- `-q` drops lock result lines but keeps errors and read values; `-qq` drops errors; exit status unchanged;
- `--dry-run`: no request reaches the mock (`expect(0)`), human and JSON lines, comment shown, works without cookies;
- `--joblog`: entries written; a rerun skips the `ok` lines and retries the failed one; an unlock run is not skipped by an earlier lock entry; a lock rerun after an unlock entry runs again; `--no-resume` runs all; a read action writes no entries;
- 429 with `Retry-After: 1` on the lookup and on the lock: the pause message, then success, one extra request each;
- a 2xx lock answer that is not JSON is success;
- the `ia account` row in `every_command_renders_its_long_help` (`ia-cli/tests/cli.rs`).

## Help and docs

`about`, `long_about` (admin-only, cookies needed, lookup-type rule, two requests per lock) and `after_long_help` examples in the `cstr!` style of `tasks.rs`. docs/usage.md: an `ia account` section after `ia tasks` with synopsis, flags table, lookup-type rule, output, bulk input, duplicates, dry run, job log (one per action; interruptions) and rate limiting; `account` added to the global `--jobs` defaults (help text in `lib.rs` and the global options table), the "Retry waits" section, the "Job logging" list and the "JSON output for agents" section; README.md command list.

## Delivery

One PR if the diff stays readable; otherwise two: the single-account port (ia-core module, positional form, every action, `--json`), then bulk input (`--itemlist`, standard input, `--jobs`, de-duplication, job log, `--dry-run`, rate-limit pause).

## Review of this plan (2026-11-10), before any code

Taken: identity check on every lookup and an item-name check before a lock; `IaError::Http` carries the whole body, as elsewhere in ia-core; both requests sent without following redirects; only the field an action uses is required; the Python-output claim corrected and the differences listed; an empty comment is not sent; lock and unlock de-duplicated by item name; resume takes the latest of lock and unlock; read actions do not use the job log; quiet levels and `std::process::exit(1)` stated; the 2-job default passed through as an `Option` and the docs that list per-command defaults; interruptions documented; JSON error body embedded without its wrapper and with `account`; `--itemlist -` with a terminal is an error; dry-run JSON has a `status`; the extra tests.

Decided by the Python client or this repository rather than left open: cookies go only to archive.org, its subdomains and loopback (the Python client scopes them to `.archive.org`); human output follows ia-cli (the issue says so); a non-JSON 2xx lock answer is success (Python); a lock is sent whatever the current state (Python); the job default is 2 with no maximum (`metadata modify`; no command has a maximum); `--itemlist -` is added for this command only, since adding it to the shared reader changes every batch command.

Kept, with reason: the wider email rule (the Python pattern sends valid addresses to the screen-name lookup, where they cannot be found); the JSON key `account` (the value is not an item identifier).

## Tasks
- [x] Plan committed first.
- [x] Plan review; findings taken or answered above.
- [x] Red: ia-core and CLI tests above.
- [x] Green: `ia_core::account`, `IaError::AccountApi`, `ia account`.
- [x] `--help`, docs/usage.md, README.md.
- [x] `cargo test -p ia-core -p ia-cli`, `cargo clippy -p ia-core -p ia-cli -- -D warnings`.
- [ ] Code review; PR.
