# How This Project Is Built

Nearly everything in this repository was written by AI agents (primarily Claude Code), directed by me ([@jjjake](https://github.com/jjjake)).
This document is no exception. It was co-written with the same agents that built the tool: the experience and opinions are mine, the drafting was shared, and the technical details (commit counts, dates, and so on) were pulled by the agents from this repository's git history, where anyone can check them. Fixes below are cited by commit, not by pull request: the issue tracker was not carried over when this code was published, so the commits are the durable record. Pull-request numbers in commit subjects, `(#N)`, refer to that unpublished tracker. The history itself was rewritten with git-filter-repo before publication to remove a colleague's name, one item identifier, and details of another team's unannounced work; commit content, authorship, and dates are otherwise unchanged.

This project is an experiment.
It's an attempt at building a next-generation archive.org tool using agentic coding tools to develop faster, keep maintenance manageable, and support future agent-first tools that interact with archive.org.
It is just an experiment, published as one.
This document is meant to share my experience with developing like this, what has worked, and what has gone wrong so far.
Why build a new tool at all, rather than improving the Python one, is covered in [Why Rust?](why-rust.md). This document is about how it's being built.

## Background

I started writing the `internetarchive` Python library and `ia` command-line tool back in 2012.
A majority of archive.org items have been created with this tool (either directly, or as a part of other tools that have incorporated it as a library).

The point being, this is not an outsider's AI-generated rewrite of an unfamiliar tool working with random APIs.
After 14 years of developing, maintaining, and using the Python library daily, across all the various archive.org services and APIs, I know this domain deeply.

This is important because the failure mode everyone understandably worries about with AI-generated code (seemingly functional code that is subtly wrong in hard-to-recognize ways) is exactly what domain knowledge protects against.
Whether it's a bug introduced by a human, or an AI agent, being able to identify wrong *behavior* is what helps build robust and reliable tools.

## Development

This is my first experience developing with Rust, and I do not read every line of code.
This is what I do instead:

- I write designs. Every big feature starts as a design document in [`docs/plans/`](plans/). This includes the architecture, the trade-offs, and the APIs. This is what drives the code.
- I file issues (usually by prompting an agent to write them up) and set priorities, so work is specified before it's implemented.
- In general, I review behavior, not code. I carefully test the tool manually against real archive.org items (the same workflows I've supported for years with the Python tool).
- Since mid-April 2026, every change to main has been a pull request gated on the checks described in the next section. The first two months were looser: 66 direct commits to main, one force-push, and 44 pull requests merged with a failing check, most of them clippy and CI-runner failures in February and March. The live-write incident below is what tightened the process.
- I continuously review the codebase with various AI review passes, with findings tracked as issues and linked in PRs. More on what some of those have found below.

The thought behind this workflow is that disciplined process can substitute for line-by-line review.
That's a bet that might not prove to be true.

## The control system

The safeguards are:

**Test-first development**: Agents are instructed, through my local agent configuration rather than anything in this repository, to write tests first and watch them fail before implementing. I don't verify that ordering on every change, and I'd guess it hasn't always been followed. What is enforced: every change lands with tests, and the full suite (1,694 hermetic tests as of September 2026) runs on every PR. The suite is a regression net, not proof of correctness.

**Hermetic tests**: The test suite sends *zero* traffic to archive.org. Every HTTP call goes through a local [`wiremock`](https://crates.io/crates/wiremock) server. (This rule has been violated; details in the next section.)

**A strict CI gate**: Five jobs on every PR: tests, `clippy` with all warnings as errors, `rustfmt`, rustdoc with warnings as errors, and `cargo-audit` for known-vulnerable dependencies. The Rust compiler itself is a meaningful first reviewer. Entire classes of bugs (memory errors, data races, unhandled error variants) fail compilation rather than reaching review at all.

**Pull requests and audit trail**: Since mid-April 2026, main accepts only pull requests. GitHub branch protection was enabled when the repository became public; it was not available on the private plan before that, and a local pre-commit hook was the only mechanical guard. About half of the pull requests link an issue in the development tracker. AI involvement is recorded per-commit in `Co-Authored-By` trailers; as of September 2026, 462 of 553 commits carry one. For any line, the design doc, the commit, and its trailer are public; the issue and pull-request discussion behind it are in the private tracker.

**Adversarial review**: Periodic multi-reviewer AI code reviews run against the whole codebase with instructions to find problems. Findings are filed as issues with severity ratings and fixed in tracked PRs.

## What has gone wrong

**Test code wrote to production archive.org.** An early version of one test ran `ia metadata nasa -m title:Test` for real. On a machine with valid credentials — mine — every test run overwrote the title of the live `nasa` item and queued the resulting catalog task. The damage was one metadata field on one item, and reversible, but it was a real write to the production catalog from automated test code. Caught and fixed in [`5040cb8`](https://github.com/internetarchivecanada/ia/commit/5040cb8b130d58f7b27d03ee63aa36de5421cae3). The May 2026 adversarial review then found four more tests making live requests. Two issued writes: on a machine with valid credentials, they would have POSTed metadata changes to the live `item2` item. The other two were read-only GETs against archive.org. All four were rerouted through mocks in [`0531563`](https://github.com/internetarchivecanada/ia/commit/0531563ed918589a2163496f15e85ec98b457cfd). This is the project's most important safety rule, and AI-written test code broke it twice.

**A feature shipped with its core check unimplemented.** The same review found that the AI metadata promotion pipeline never actually enforced its confidence threshold. The flag existed, was documented, and did nothing (fixed in [`cd6299b`](https://github.com/internetarchivecanada/ia/commit/cd6299bf1f970e3863754dea34cffcb3dc52bd91)).

**Thirteen high-severity findings in total** from that review — missing HTTP timeouts, a rate-limiter race condition, a panic path on a zero-size edge case, error-classification bugs — resolved across [`d2d8a4f`](https://github.com/internetarchivecanada/ia/commit/d2d8a4f5d9e6b15197c76b8ca89b2c5315fde681), [`238a497`](https://github.com/internetarchivecanada/ia/commit/238a4979b5beb9042e6116fc296a8490a0761797), [`78fb072`](https://github.com/internetarchivecanada/ia/commit/78fb072a36943dfc5de09575fa3531d2fc8c5ea5) and [`f46ad2c`](https://github.com/internetarchivecanada/ia/commit/f46ad2cc80ffc4c866653133ddae251ed108196a), with two CLI-consistency items still open. The review rated them high; I consider them lower-risk, and they are still open.

**The Rust port initially had the same vulnerability class as the Python CVE.** The Python library had a critical path-traversal vulnerability ([CVE-2025-58438](https://nvd.nist.gov/vuln/detail/CVE-2025-58438)) in downloads. A dedicated [security audit](security/2026-03-03-download-security-audit.md) of the Rust port found it needed the same guards (path validation, symlink checks, a resume-time race fix) which were implemented in [`6b98db2`](https://github.com/internetarchivecanada/ia/commit/6b98db20b87de12717d014a1af2cbd159ba898c8) and shipped in v0.4.4. Nine earlier tagged builds, reachable only by the private repository's four collaborators, carried the same vulnerability class.

**Other things the record shows.** Both bugs above shipped in tagged releases before they were found: the live-write test in five, the threshold bug in three. Every download incremented archive.org's public view counter until 2026-06-01, when `cnt=0` was added to download requests. A larger automated review round on 2026-06-11 produced 72 unverified findings, was cut off by a spending limit, and was never filed or resumed; its one critical finding, that multipart-upload resume trusts existing parts without checking their integrity, matches an issue open since March 2026 and still open. The tracker does not record whether the nasa title from the first incident was restored.

Checking for the live-test class is mechanical, and you can do it yourself: run the suite with outbound HTTP blocked and only loopback allowed —

```sh
HTTP_PROXY=http://127.0.0.1:1 HTTPS_PROXY=http://127.0.0.1:1 NO_PROXY=localhost,127.0.0.1 cargo test
```

— and any test that depends on the real network fails. Running exactly that check while writing this document found one more non-hermetic test (read-only GETs to archive.org; the review had flagged it as a medium finding that was never filed, and [`0531563`](https://github.com/internetarchivecanada/ia/commit/0531563ed918589a2163496f15e85ec98b457cfd) did not touch it), fixed in [`7b81199`](https://github.com/internetarchivecanada/ia/commit/7b8119943cb7db5c31587bceaafc75b696a2be60) along with a [`scripts/hermetic-audit`](../scripts/hermetic-audit) script that automates the check and scans every version of the test code ever committed.

The check has a known gap: it cannot see a request that a test tolerates failing, because such a test passes either way. An independent pre-publication review in September 2026 measured that gap by running the suite through a local logging proxy. Twenty-three tests made read-only requests to archive.org on every run, across upload dry-run, verify, download, and metadata export. Seven `metadata modify` and `metadata import` tests in `ia-cli/tests/cli.rs` would have sent real writes from any machine with credentials; they passed only because the test machine had none, and their own comments said so. The audit script's static mode had listed every one of them, in a review list nobody worked through. Fixed in [`eae6a17`](https://github.com/internetarchivecanada/ia/commit/eae6a17) by giving every test helper, and the library's dry-run test client, a config that loads no credentials and points at a port that refuses connections. Running the suite through the logging proxy again afterward recorded zero connection attempts. That is the third time this rule was broken, and the first time it was caught before it wrote anything.

What I take from this record: the first-pass code is good but not trustworthy on its own; the review, audit, and careful hands-on manual testing layer has real teeth; and the gap between those is exactly why this project is labeled alpha.

## Known weaknesses

Things that are weak right now, independent of anything a review has caught. Possible remedies are noted where I have one; some of these don't have clear answers yet:

- **I can't judge the Rust myself yet.** This is my first Rust project, and clippy and the compiler are standing in for the judgment I'd have in Python. The reviews have caught Rust-specific problems (blocking I/O on the async runtime, panic paths), but the remedy I'd most like to find is a proper one-time architectural review by someone experienced in Rust.
- **Every automated layer is still AI.** The reviews that found the bugs above were AI passes with different objectives, which is not the same as independent human judgment. No human has read most of this code. The architectural review above would help here too, and a human pass over the write paths would be worth more than another automated one.
- **The strength of the test suite is unmeasured.** 1,700 tests says nothing about how many would catch a real bug. Mutation testing (`cargo-mutants`) seeds bugs and counts how many the suite kills. Worth running and publishing the score, whatever it turns out to be.
- **Some test fixtures are circular.** The mocked API responses are the spec, and in places the same agent wrote both the fixture and the code it validates. Fixtures captured from real API responses are much stronger. The write paths should get that treatment first.
- **Bugs invisible to my manual testing survive longest.** The confidence-threshold bug above lived as long as it did because nothing I'd naturally look at would reveal it. The fix is a workflow rule, not more tests: for write-path or safety-relevant features, the agent demonstrates the behavior end-to-end (dry-run output, joblogs, before/after diffs) and I review that.
- **The review cadence is ad hoc.** The adversarial reviews have caught real bugs, but when they run and what they cover is informal. Scheduling and scoping them would make the safety claim stronger.

## FAQ

**"Nobody reads the code line-by-line. Isn't that reckless?"**
It very well could be!
This is an experiment to help answer that question.
The idea is not that review is unnecessary.
It's that for this codebase, behavior-level review by a domain expert, plus a compiler that rejects whole bug classes, plus a hard CI gate, plus periodic adversarial review, catches more than my eyes skimming a 600-line diff would.
And while this library interacts with very sensitive endpoints that have the ability to delete data, it's made up of tried-and-true patterns informed by and ported from a mature codebase (the Python library).
This is a very important question that I am interested in hearing from others on.

**"The same AI writes the tests. Don't they share blind spots?"**
Yes, demonstrably. The confidence-threshold bug above is exactly that. Tests written alongside an implementation inherit its assumptions. That's why the layers outside that loop matter most: review passes with a different objective, security audits against known CVE classes, and my own hands-on testing against real workflows. No single layer is enough on its own.

**"What happens if the AI tools go away?"**
The code doesn't depend on the tools that wrote it.
It's ordinary Rust: a Cargo workspace, documented public APIs, strict lints, a conventional test suite.
Any Rust developer can maintain it with no AI involvement whatsoever.
The repository's [AGENTS.md](../AGENTS.md) exists to help AI tools work on the codebase, but nothing requires them.

**"Why develop this way at all?"**
Two reasons. First, leverage: I could not have built a tool of this scope solo, in my available time, by hand. It's 553 commits, most of them in February and March 2026 with a slower tail through September, alongside maintaining the Python library. Second, the experiment itself: can someone with deep domain knowledge direct AI implementation, under real guardrails, and end up with a tool they trust? This repository is my attempt at finding out.

**"Why not just keep improving the Python tool?"**
The short version: the changes that matter (the concurrency model, connection handling, single-binary distribution) are rewrite-scale either way. See [Why Rust?](why-rust.md) for the full reasoning, including the trade-offs and the alternatives considered. The Python tool is still maintained and not going anywhere.

**"Should anything at the Internet Archive depend on this today?"**
No. It's alpha and says so in the README. The bar for trusting it is the same as for any young tool, regardless of authorship: does it behave correctly against *your* workflows, and does the record (this document, the commit log, and the issue tracker as it fills) show problems being found and fixed. The Python `internetarchive` library remains the mature, stable option, and it is not going anywhere.

**"Is this an argument that other projects should be built this way?"**
No. It's one project, one maintainer, one domain, with an unusual amount of pre-existing domain expertise behind it. I have no idea whether this approach generalizes.

## Feedback

I would love feedback on any of this — open an issue on this repository.

If you're thinking about contributing code, open an issue first. This is an experiment, and I don't want anyone wasting their time on work that turns out to conflict with where it's going.

Security issues go to info@archive.org rather than the tracker; see [SECURITY.md](../SECURITY.md).

## Further reading

- [Why Rust?](why-rust.md) — the language choice, including its trade-offs
- [Design philosophy](design-philosophy.md) — design principles and architecture
- [CONTRIBUTING.md](../CONTRIBUTING.md) — the practical workflow, for humans and AI tools
