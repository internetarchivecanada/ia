#[cfg(not(feature = "self-update"))]
mod without_feature {
    use assert_cmd::Command;
    use predicates::prelude::*;

    fn ia() -> Command {
        assert_cmd::cargo_bin_cmd!("ia-cli")
    }

    /// When built without the self-update feature (the default for tests),
    /// `ia update` should not be a recognized subcommand.
    #[test]
    fn update_command_not_available_without_feature() {
        ia().arg("update")
            .assert()
            .failure()
            .stderr(predicate::str::contains("unrecognized subcommand"));
    }

    #[test]
    fn update_list_not_available_without_feature() {
        ia().args(["update", "list"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("unrecognized subcommand"));
    }

    #[test]
    fn update_install_not_available_without_feature() {
        ia().args(["update", "install", "0.5.0"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("unrecognized subcommand"));
    }
}

#[cfg(feature = "self-update")]
mod with_feature {
    use assert_cmd::Command;
    use predicates::prelude::*;

    fn ia() -> Command {
        assert_cmd::cargo_bin_cmd!("ia-cli")
    }

    // --- install: below-minimum version floor ---

    #[test]
    fn install_below_minimum_shows_error() {
        ia().args(["update", "install", "0.1.0"])
            .assert()
            .failure()
            .stderr(predicate::str::contains(
                "below minimum installable version",
            ));
    }

    #[test]
    fn install_below_minimum_json_output() {
        ia().args(["update", "install", "0.1.0", "--json"])
            .assert()
            .failure()
            .stderr(
                predicate::str::contains("update_below_minimum")
                    .and(predicate::str::contains("0.1.0"))
                    .and(predicate::str::contains(
                        ia_core::update::MIN_INSTALLABLE_VERSION,
                    )),
            );
    }

    #[test]
    fn install_below_minimum_no_installing_message() {
        // The "Installing..." progress message should NOT appear when the
        // version is rejected by the floor check.
        ia().args(["update", "install", "0.1.0"])
            .assert()
            .failure()
            .stdout(predicate::str::contains("Installing").not());
    }

    // --- parent --json propagation ---

    #[test]
    fn parent_json_flag_propagates_to_install() {
        // `ia update --json install 0.1.0` should produce JSON error on stderr,
        // same as `ia update install 0.1.0 --json`.
        ia().args(["update", "--json", "install", "0.1.0"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("update_below_minimum"));
    }

    // --- help text ---

    #[test]
    fn update_list_help() {
        ia().args(["update", "list", "--help"])
            .assert()
            .success()
            .stdout(
                predicate::str::contains("List available versions")
                    .and(predicate::str::contains("--all"))
                    .and(predicate::str::contains("--json")),
            );
    }

    #[test]
    fn update_install_help() {
        ia().args(["update", "install", "--help"])
            .assert()
            .success()
            .stdout(
                predicate::str::contains("Install a specific version")
                    .and(predicate::str::contains("--json")),
            );
    }

    // --- install: version argument required ---

    #[test]
    fn install_requires_version_argument() {
        ia().args(["update", "install"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("<VERSION>").or(predicate::str::contains("required")));
    }
}
