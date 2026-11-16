#!/bin/sh
# shellcheck disable=SC2016 # sh -c scripts expand their own $1
# Smoke test for install.sh against the real GitHub release. Downloads the
# release binary for this computer, so it needs the network; it is separate
# from the hermetic cargo tests and from scripts/test-install.sh.
#
# For each login-shell case (zsh, bash without ~/.bash_profile, bash with
# ~/.bash_profile) it installs into a fresh temporary HOME, installs again
# with the script piped to sh as the README command does, and checks that the
# binary runs and the profile has exactly one added line that puts it on
# PATH. Also installs one pinned release with IA_VERSION.
#
# Usage: scripts/test-install-live.sh

set -eu

repo=$(cd "$(dirname "$0")/.." && pwd)
installer="$repo/install.sh"
work=$(mktemp -d "${TMPDIR:-/tmp}/ia-install-live.XXXXXX")
work=$(cd "$work" && pwd -P) # no // or symlinks, so shells print the same path
trap 'rm -rf "$work"' EXIT
trap 'exit 1' HUP INT TERM

failures=0

check() { # description, command...
    desc=$1
    shift
    if "$@"; then
        printf 'ok   %s\n' "$desc"
    else
        printf 'FAIL %s\n' "$desc"
        failures=$((failures + 1))
    fi
}

clean_env() { # HOME SHELL command...: run with only what a new Terminal has
    h=$1
    s=$2
    shift 2
    env -i HOME="$h" SHELL="$s" PATH="/usr/bin:/bin:/usr/sbin:/sbin" TMPDIR="$work" "$@"
}

runs() { # binary: --version prints "ia-cli X.Y.Z"
    "$1" --version | grep -q '^ia-cli [0-9]'
}

one_line() { # file
    [ "$(grep -c 'added by the ia-cli installer' "$1")" = 1 ]
}

on_path_after_login() { # HOME profile: sourcing the profile finds the binary
    # shellcheck disable=SC2016 # expanded by the inner sh
    env -i HOME="$1" PATH=/usr/bin:/bin sh -c '. "$2"; [ "$(command -v ia-cli)" = "$1/bin/ia-cli" ]' _ "$1" "$2"
}

run_case() { # name SHELL profile-to-create profile-expected
    name=$1
    shell=$2
    h="$work/home $name"
    mkdir -p "$h"
    if [ -n "$3" ]; then
        : >"$h/$3"
    fi
    profile="$h/$4"

    printf '\n== %s: fresh install ==\n' "$name"
    clean_env "$h" "$shell" sh "$installer"
    check "$name fresh: binary runs" runs "$h/bin/ia-cli"
    check "$name fresh: one line in $4" one_line "$profile"
    if [ "$name" != fish ]; then
        check "$name fresh: the line puts ia-cli on PATH" on_path_after_login "$h" "$profile"
    fi

    printf '\n== %s: second run, piped to sh ==\n' "$name"
    clean_env "$h" "$shell" sh <"$installer"
    check "$name rerun: binary runs" runs "$h/bin/ia-cli"
    check "$name rerun: still one line in $4" one_line "$profile"
}

# finds_ia_cli HOME SHELL ARGS...: a new shell started that way, with only
# HOME and a minimal PATH, finds HOME/bin/ia-cli.
finds_ia_cli() {
    fh=$1
    shell_path=$(command -v "$2")
    shift 2
    [ "$(env -i HOME="$fh" PATH=/usr/bin:/bin TERM=dumb "$shell_path" "$@" 'command -v ia-cli' 2>/dev/null </dev/null | tail -n 1)" = "$fh/bin/ia-cli" ]
}

run_case zsh /bin/zsh "" .zprofile
check "zsh: one line in .zshrc" one_line "$h/.zshrc"
run_case bash-profile /bin/bash "" .profile
check "bash: one line in .bashrc" one_line "$h/.bashrc"
if command -v bash >/dev/null 2>&1; then
    check "bash: a new interactive non-login bash finds ia-cli" finds_ia_cli "$h" bash -i -c
fi
run_case bash-bash_profile /bin/bash .bash_profile .bash_profile

if command -v fish >/dev/null 2>&1; then
    run_case fish /usr/bin/fish "" .config/fish/conf.d/ia-cli.fish
    check "fish: a new login fish finds ia-cli" finds_ia_cli "$h" fish -l -c
    check "fish: a new non-login fish finds ia-cli" finds_ia_cli "$h" fish -c
else
    printf '\n(fish is not installed; fish case skipped)\n'
fi

printf '\n== pinned release: IA_VERSION=v0.27.0 ==\n'
h="$work/home pinned"
mkdir -p "$h"
clean_env "$h" /bin/zsh IA_VERSION=v0.27.0 sh "$installer"
check "pinned: installs 0.27.0" sh -c '[ "$("$1" --version)" = "ia-cli 0.27.0" ]' _ "$h/bin/ia-cli"

printf '\n%s failed\n' "$failures"
[ "$failures" -eq 0 ]
