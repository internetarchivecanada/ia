# Upload Dead-Send Rule Implementation Plan (after #41)

**Decision (Jake, 2026-10-02):** remove `--min-speed` from upload. #41 mirrored download's interface, but a rate floor has no purpose for an upload: a re-send goes to the same endpoint, so "slow" is not a signal of anything fixable, only "dead" is, and "dead" is a fixed rule, not a tunable. The 10 KiB/s default would also have abandoned a legitimately slow uplink that was still moving bytes.

**Rule.** A body send that moves no bytes for 60 s is abandoned (the connection is closed) and re-sent at once, spending one of `--retries`. The stall detector does this with a floor of one byte per second and both its window and grace set to 60 s, so a send dead from its first poll is caught at 60 s and a send that dies after progress is caught about 60 s after its last byte. Nothing is exposed: no flag, no `UploadOpts` field.

**Changes.**
- `UploadOpts.min_speed` and `UploadOptsBuilder::min_speed` removed. `S3RetryCtx.min_speed` removed. `BodyWatch::new()` takes nothing; `is_active` and the zero-floor path go.
- `IaError::UploadStalled { identifier, key, window_secs, stalls }`: "upload of {identifier}/{key} stalled {stalls} time(s): no bytes were sent for {window_secs} s". JSON `upload_stalled` with those fields.
- `KeptUpload::describe`: "part N of M stalled K times (no bytes sent for W s, after A attempts)".
- CLI: `--min-speed` removed from both upload structs; the `--retries` help says a stall (a send that moves no bytes for 60 s) spends one, and the `long_about` states the rule in one sentence; the example goes. `commands::rate` stays (download uses it).
- usage.md: the `--min-speed` row goes; "Slow and stalled uploads" becomes "Stalled uploads", stating the rule, the final error, and the kept multipart upload; the part-failures stall sentence follows the new wording.
- The #38 plan carries a dated note.

**Tests.** Red first: `UploadOpts` has no `min_speed` (compile), `upload --help` has no `--min-speed` and states "moves no bytes for 60 s", `--min-speed` is rejected as an unknown argument, the error display and JSON carry the new fields, the stalled-part message reads "no bytes sent for". The stall tests keep passing with the shrunk policy (the upload watch then uses 2 s for both window and grace).

- [ ] Tests, red; implement; `just ci`; review; PR; squash-merge; `scripts/ia-cleanup upload-dead-send` only after a confirmed merge.
