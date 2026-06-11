# How This Project Is Built

Nearly everything in this repository was written by AI agents (primarily Claude Code), directed by me ([@jjjake](https://github.com/jjjake)).
This document is no exception. It was co-written with the same agents that built the tool: the experience and opinions are mine, the drafting was shared, and the technical details (commit counts, PR numbers, etc.) were pulled by the agents from the git history and issue tracker, where anyone can verify them.

This project is a true experiment.
It's an attempt at building a next-generation archive.org tool using agentic coding tools to develop faster, keep maintenance manageable, and support future agent-first tools that interact with archive.org.
It is just an experiment, and there is no commitment to a public release.
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
- I gate every merge on the checks described in the next section. Nothing lands on main without a passing PR.
- I continuously review the codebase with various AI review passes, with findings tracked as issues and linked in PRs. More on what some of those have found below.

The thought behind this workflow is that disciplined process can substitute for line-by-line review.
That's a bet that might not prove to be true.

## The control system

The safeguards are:

**Test-first development**: Agents are instructed to write tests first and watch them fail before implementing. I don't verify that ordering on every change, and I'd guess it hasn't always been followed. What is enforced: every change lands with tests, and the full suite (~1,700 hermetic tests as of June 2026) runs on every PR. The suite is a regression net, not proof of correctness.

**Hermetic tests**: The test suite sends *zero* traffic to archive.org. Every HTTP call goes through a local [`wiremock`](https://crates.io/crates/wiremock) server. (This rule has been violated; details in the next section.)

**A strict CI gate**: Five jobs on every PR: tests, `clippy` with all warnings as errors, `rustfmt`, rustdoc with warnings as errors, and `cargo-audit` for known-vulnerable dependencies. The Rust compiler itself is a meaningful first reviewer. Entire classes of bugs (memory errors, data races, unhandled error variants) fail compilation rather than reaching review at all.

**Branch protection and audit trail**: No commits to main; everything is a PR linked to an issue. AI involvement is recorded per-commit in `Co-Authored-By` trailers; as of June 2026, 450 of 539 commits carry one. The full chain from design doc to issue to PR to commit is reconstructible for any line in the codebase.

**Adversarial review**: Periodic multi-reviewer AI code reviews run against the whole codebase with instructions to find problems. Findings are filed as issues with severity ratings and fixed in tracked PRs.

## What has gone wrong

**Test code wrote to production archive.org.** An early version of one test ran `ia metadata nasa -m title:Test` for real. On a machine with valid credentials — mine — every test run overwrote the title of the live `nasa` item and queued the resulting catalog task. The damage was one metadata field on one item, and reversible, but it was a real write to the production catalog from automated test code. Caught and fixed in [#339](https://github.com/jjjake/ia/pull/339). The May 2026 adversarial review then found four more tests making live requests. Two issued writes: on a machine with valid credentials, they would have POSTed metadata changes to the live `item2` item. The other two were read-only GETs against archive.org. All four were rerouted through mocks in [#343](https://github.com/jjjake/ia/pull/343) ([#342](https://github.com/jjjake/ia/issues/342)). This is the project's most important safety rule, and AI-written test code broke it twice.

**A feature shipped with its core check unimplemented.** The same review found that the AI metadata promotion pipeline never actually enforced its confidence threshold. The flag existed, was documented, and did nothing ([#349](https://github.com/jjjake/ia/issues/349), fixed in [#360](https://github.com/jjjake/ia/pull/360)).

**Thirteen high-severity findings in total** from that review — missing HTTP timeouts, a rate-limiter race condition, a panic path on a zero-size edge case, error-classification bugs — tracked as issues [#346](https://github.com/jjjake/ia/issues/346)–[#358](https://github.com/jjjake/ia/issues/358) and resolved across PRs [#359](https://github.com/jjjake/ia/pull/359)–[#363](https://github.com/jjjake/ia/pull/363), with two lower-risk CLI-consistency items still open.

**The Rust port initially had the same vulnerability class as the Python CVE.** The Python library had a critical path-traversal vulnerability ([CVE-2025-58438](https://nvd.nist.gov/vuln/detail/CVE-2025-58438)) in downloads. A dedicated [security audit](security/2026-03-03-download-security-audit.md) of the Rust port found it needed the same guards (path validation, symlink checks, a resume-time race fix) which were implemented in [#177](https://github.com/jjjake/ia/pull/177), long before this tool could end up in anyone else's hands.

Checking for the live-test class is mechanical, and you can do it yourself: run the suite with outbound HTTP blocked and only loopback allowed —

```sh
HTTP_PROXY=http://127.0.0.1:1 HTTPS_PROXY=http://127.0.0.1:1 NO_PROXY=localhost,127.0.0.1 cargo test
```

— and any test that depends on the real network fails. Running exactly that check while writing this document found one more non-hermetic test (read-only GETs to archive.org, missed by both the review and [#343](https://github.com/jjjake/ia/pull/343)), fixed in [#375](https://github.com/jjjake/ia/pull/375) along with a [`scripts/hermetic-audit`](../scripts/hermetic-audit) script that automates the check and scans every version of the test code ever committed. One known gap remains: the check can't see requests a test tolerates failing, and a handful of read-only export tests still send those ([#376](https://github.com/jjjake/ia/issues/376)).

What I take from this record: the first-pass code is good but not trustworthy on its own; the review, audit, and careful hands-on manual testing layer has real teeth; and the gap between those is exactly why this project is labeled alpha and not public.

## Known weaknesses

Things that are weak right now, independent of anything a review has caught. Possible remedies are noted where I have one; some of these don't have clear answers yet:

- **I can't judge the Rust myself yet.** This is my first Rust project, and clippy and the compiler are standing in for the judgment I'd have in Python. The reviews have caught Rust-specific problems (blocking I/O on the async runtime, panic paths), but the remedy I'd most like to find is a proper one-time architectural review by someone experienced in Rust.
- **Every automated layer is still AI.** The reviews that found the bugs above were AI passes with different objectives, which is not the same as independent human judgment. No human has read most of this code. The architectural review above would help here too, and a human pass over the write paths would be worth more than another automated one.
- **The strength of the test suite is unmeasured.** 1,700 tests says nothing about how many would catch a real bug. Mutation testing (`cargo-mutants`) seeds bugs and counts how many the suite kills. Worth running and publishing the score, whatever it turns out to be.
- **Some test fixtures are circular.** The mocked API responses are the spec, and in places the same agent wrote both the fixture and the code it validates. Fixtures captured from real API responses are much stronger. The write paths should get that treatment first.
- **Bugs invisible to my manual testing survive longest.** The confidence-threshold bug ([#349](https://github.com/jjjake/ia/issues/349)) lived as long as it did because nothing I'd naturally look at would reveal it. The fix is a workflow rule, not more tests: for write-path or safety-relevant features, the agent demonstrates the behavior end-to-end (dry-run output, joblogs, before/after diffs) and I review that.
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
Two reasons. First, leverage: I could not have built a tool of this scope solo, in my available time, by hand. It's roughly 540 commits and 146 merged PRs in four months, alongside maintaining the Python library. Second, the experiment itself: can someone with deep domain knowledge direct AI implementation, under real guardrails, and end up with a tool they trust? This repository is my attempt at finding out.

**"Why not just keep improving the Python tool?"**
The short version: the changes that matter (the concurrency model, connection handling, single-binary distribution) are rewrite-scale either way. See [Why Rust?](why-rust.md) for the full reasoning, including the trade-offs and the alternatives considered. The Python tool is still maintained and not going anywhere.

**"Should anything at the Internet Archive depend on this today?"**
No. It's alpha and says so in the README. The bar for trusting it is the same as for any young tool, regardless of authorship: does it behave correctly against *your* workflows, and does the issue tracker show problems being found and fixed. The Python `internetarchive` library remains the mature, stable option, and it is not going anywhere.

**"Is this an argument that other projects should be built this way?"**
No. It's one project, one maintainer, one domain, with an unusual amount of pre-existing domain expertise behind it. I have no idea whether this approach generalizes.

## Feedback

I would love feedback on any of this. Reach out to me.

If you're thinking about contributing code, please talk to me first. This is an experiment, and I don't want anyone wasting their time without coordinating.

## Further reading

- [Why Rust?](why-rust.md) — the language choice, including its trade-offs
- [Design philosophy](design-philosophy.md) — design principles and architecture
- [CONTRIBUTING.md](../CONTRIBUTING.md) — the practical workflow, for humans and AI tools
