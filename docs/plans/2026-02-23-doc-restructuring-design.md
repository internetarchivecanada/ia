# Documentation Restructuring

**Date**: 2026-02-23
**Status**: Approved

## Problem

`docs/why-rust.md` is doing double duty as both a sales pitch ("why should I care?") and a design reference ("how does it work?"). The result is a doc that's too detailed to hook someone quickly and too scattered to serve as a design reference. There's no single place that synthesizes the design principles spread across 15+ plan docs in `docs/plans/`.

## Solution

Split into two docs with clear purposes:

### `docs/why-rust.md` — The pitch

Slimmed down, confident, factual. Answers "why does this exist and why should I care?" Keeps the three-audience thesis as its anchor:

> The Python CLI was designed for humans at a terminal. The Rust CLI is designed for three audiences: humans, AI coding agents, and machine consumers. Everything below follows from that.

Covers headline wins only: concurrency, single binary, modern UX, agent/machine native. No implementation details (no semaphore strategy, no reqwest internals, no `.part` file mechanics). ~40-50 lines.

### `docs/design-philosophy.md` — The design reference

Synthesizes core design principles from across all plan docs. Serves both human contributors and AI agents working on the codebase. ~150-200 lines. Sections:

1. **Three audiences** — Expands the thesis. Defines each audience concretely (humans at terminals, AI agents that shell out and parse stdout, machine consumers like MCP servers and pipelines). How the CLI serves all three simultaneously.

2. **CLI as platform** — The aspirational case for the CLI replacing the Python library. Language-agnostic integration via `--json`. Acknowledges trade-offs (process spawn latency, no in-process callbacks) and where a native library still wins. Framed as a goal we're working toward, not a claim that the Python library is unnecessary today.

3. **Library-first architecture** — `ia-core` is a real library, not CLI internals exposed. The CLI is a thin presentation layer. The GUI crate proves the library works independently. For people who need in-process integration, the Rust library exists.

4. **Output design** — Two modes: human (colored, progress bars, tables) and machine (`--json`). Same data, different presentation. Links to agent-friendly output design doc.

5. **HTTP excellence** — Connection pooling, keep-alive, retry with backoff, rate limit coordination, byte-range resume. Links to rust-port design doc.

6. **Error handling** — Typed errors with stable codes. Binary exit codes (0/1). Structured stderr. Machines match on codes, humans get readable messages.

7. **Concurrency model** — Shared semaphore, async throughout, JSONL streaming for batch ops. Links to relevant plan docs.

8. **Further reading** — Map of all plan docs with one-line descriptions.

### README change

Link bar changes from:

```
Why Rust? · Usage guide
```

To:

```
Why Rust? · Design philosophy · Usage guide
```

No other README changes.

## Deliverables

1. Slim `docs/why-rust.md` — remove implementation details, keep pitch tight
2. Create `docs/design-philosophy.md` — synthesize design principles, link to plan docs
3. Update `README.md` link bar — add design philosophy link
