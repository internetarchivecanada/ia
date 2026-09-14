# Why Rust?

A disclosure first: the maintainer of this project, [@jjjake](https://github.com/jjjake), also wrote and maintains the Python [`internetarchive`](https://github.com/jjjake/internetarchive) library and its `ia` CLI. Everything critical said below about the Python tool is self-critique, informed by more than a decade of maintaining it.

Second: **the Python library remains maintained, and this project does not deprecate it.** It has a large install base, it works, and nobody is required to switch. This is a parallel implementation that has to earn trust on its own merits.

With that said — why rewrite, and why in Rust?

## HTTP robustness

The Internet Archive serves petabytes across distributed infrastructure. For a bulk-transfer tool, the HTTP layer is the product.

The Python `internetarchive` library explicitly sets `Connection: close` on every request, forcing a fresh TCP+TLS handshake per file — even when downloading thousands of files from the same server. This is a deliberate, defensive choice: `urllib3`'s connection pool struggles with stale keep-alive connections on long-running batch jobs, producing cryptic `ConnectionError` and `ChunkedEncodingError` failures. Closing every connection trades throughput for reliability.

Could this be fixed in Python? In principle, yes — modern Python HTTP stacks (httpx, aiohttp) exist. In practice it means rewriting the transfer core of a mature codebase whose chief virtue is stability, and migrating its concurrency model along the way. That's rewrite-scale work either way. Given a rewrite, it's worth picking the stack with the strongest foundation: hyper/reqwest manage connection pools robustly by default.

Beyond connection handling:

- **Concurrent transfers.** The Python CLI downloads one file at a time. The Rust version transfers files in parallel (`--jobs`), with a shared rate-limiter that pauses *all* workers when the server says slow down.
- **Byte-range resume.** Interrupted downloads continue where they left off, with checksum verification.
- **`Expect: 100-continue` for uploads.** `requests`/`urllib3` don't support it; hyper does natively — avoiding sending a multi-GB body only to receive a 4xx rejection.

## Single binary

The Python CLI requires a working Python environment. Version conflicts, broken pip installs, and virtualenv confusion are real support burdens — especially for non-developer users like archivists and librarians.

The Rust version compiles to a single static binary for Linux (x86_64), macOS (Apple Silicon), and Windows (x86_64). Download, make executable, run. No package manager required.

## Agent and machine native

The Python CLI was designed for humans at a terminal. This CLI is designed for three audiences: humans, AI agents, and machine consumers. Every command supports `--json`; MCP tool servers can wrap each command as a typed tool; pipelines consume JSONL streams. The CLI is a language-agnostic integration surface — anything that can spawn a process and read stdout can use it.

Rust also suits how this project is actually developed — implementation written by AI agents under human direction, where the compiler acts as a strict first reviewer. That's a large enough topic to get its own document: [How this project is built](ai-development.md).

## What Rust actually buys — and what it doesn't

Being precise here matters, because overclaiming is how trust gets overdrawn.

**Language guarantees** (hold by construction):

- **No data races.** Concurrent download/upload workers are safe because the borrow checker enforces exclusive access at compile time.
- **Typed, exhaustive errors.** Errors are typed (`IaError`), and exhaustive `match` means adding a new error variant forces every match site to handle it. This does not stop code from discarding a `Result`; catching that is review, not the compiler.
- **No null-reference or use-after-free bugs.** Whole bug classes fail compilation instead of reaching review.

**Not language guarantees** (process, not Rust):

- **Path traversal.** The Python library had a critical path-traversal CVE in downloads ([CVE-2025-58438](https://nvd.nist.gov/vuln/detail/CVE-2025-58438), fixed in 5.5.1). The Rust port initially needed the same guards — a dedicated [security audit](security/2026-03-03-download-security-audit.md) found and fixed them ([`6b98db2`](https://github.com/internetarchivecanada/ia/commit/6b98db20b87de12717d014a1af2cbd159ba898c8)) in v0.4.4, before the tool was available outside the private development repository. Rust didn't prevent that bug class; the audit did.
- **Logic bugs.** Rust can't tell you that a documented confidence threshold is never enforced. Tests, reviews, and audits do that — see [How this project is built](ai-development.md).

**Came along with the rewrite** (honestly: achievable in Python too, with rich/textual and friends — these are benefits of starting fresh, not arguments for Rust):

- Progress bars for files and batches; colored output inspired by [uv](https://github.com/astral-sh/uv)
- Full-screen TUI dashboard (`--dashboard`) for batch operations
- JSONL job logging (`--joblog`) with automatic resume
- Multi-disk pool support for large downloads

## Alternatives considered

**Improve the Python CLI instead?** Some fixes do land there — the path-traversal CVE was fixed in the Python library, and it continues to be maintained. But the structural items — the concurrency model, connection reuse under `requests`/`urllib3`, single-binary distribution — are rewrite-scale changes inside a codebase whose users value it precisely for not changing. A rewrite-in-place would destabilize the stable tool. A parallel implementation lets the Python tool stay what it is while the new one earns trust separately.

**Why not Go?** Go would be a credible choice: single binaries, a solid HTTP stack, fast builds, an easier learning curve. Anyone who would have picked Go is making a defensible call, and most of the gap is preference-sized. Rust was chosen for the axes weighted heaviest here: exhaustive error handling enforced at compile time, a type system that converts more mistakes into compile errors, and the hyper/tokio/ratatui ecosystem. The compile-time strictness matters double under this project's development model, where the compiler is the first reviewer of AI-written code.

## Trade-offs

Honest costs of this choice:

- **Smaller contributor pool.** The Internet Archive is largely a Python shop. A Rust codebase raises the bar for casual internal contribution. Partial mitigation: most *integration* doesn't require writing Rust — the CLI's `--json` surface is consumable from any language, and `ia-core` exists for Rust consumers.
- **A rewrite discards battle-tested behavior.** The Python code embodies a decade of accumulated knowledge about archive.org API quirks — edge cases no documentation records. The port mitigates this by studying the Python source directly and encoding known quirks into hermetic tests, but some hard-won handling will inevitably be relearned the hard way. This is a core reason the project is labeled alpha.
- **Slower iteration.** Compile times are real, and the language is stricter about everything — that's the point, but it has a cost.
- **Two tools to maintain** for the foreseeable future.

These costs were accepted knowingly. Whether the bet pays off is what the alpha period is for.

## Further reading

- [How this project is built](ai-development.md) — the AI-assisted development experiment, its safeguards, and its failures
- [Design philosophy](design-philosophy.md) — core design principles and architecture
- [Usage guide](usage.md) — quick start, configuration, advanced features
- [Full design document](plans/2026-02-20-ia-rust-port-design.md) — architecture and rationale
