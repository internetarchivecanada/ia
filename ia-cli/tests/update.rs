use assert_cmd::Command;
use predicates::prelude::*;

fn ia() -> Command {
    assert_cmd::cargo_bin_cmd!("ia")
}

/// When built without the self-update feature (the default for tests),
/// `ia update` should not be a recognized subcommand.
#[test]
#[cfg(not(feature = "self-update"))]
fn update_command_not_available_without_feature() {
    ia().arg("update")
        .assert()
        .failure()
        .stderr(predicate::str::contains("unrecognized subcommand"));
}
