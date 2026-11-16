#!/bin/sh
# shellcheck disable=SC2016 # sh -c scripts expand their own $1
# Smoke test for install.sh against the real GitHub release. Downloads the
# release binary for this computer, so it needs the network; it is separate
# from the hermetic cargo tests and from scripts/test-install.sh.
#
# For SHELL set to zsh, bash and fish, and unset, it installs into a fresh
# temporary HOME, checks that new zsh, bash and fish shells (login and not)
# find ia-cli, installs again with the script piped to sh as the README
# command does, and checks each file has exactly one added line. Also
# installs one pinned release with IA_VERSION.
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

runs() { # binary: --version prints "ia-cli X.Y.Z"
    "$1" --version | grep -q '^ia-cli [0-9]'
}

one_line() { # file
    [ "$(grep -c 'added by the ia-cli installer' "$1")" = 1 ]
}

# finds_ia_cli HOME SHELL ARGS...: a new shell started that way, with only
# HOME and a minimal PATH, finds HOME/bin/ia-cli.
finds_ia_cli() {
    fh=$1
    shell_path=$(command -v "$2")
    shift 2
    [ "$(env -i HOME="$fh" PATH=/usr/bin:/bin TERM=dumb "$shell_path" "$@" 'command -v ia-cli' 2>/dev/null </dev/null | tail -n 1)" = "$fh/bin/ia-cli" ]
}

# The installer sets PATH for zsh, bash and fish when it can find them.
base_path=/usr/bin:/bin:/usr/sbin:/sbin
for s in zsh bash fish; do
    p=$(command -v "$s" 2>/dev/null || true)
    if [ -n "$p" ]; then
        base_path="$base_path:${p%/*}"
    fi
done

run_case() { # SHELL value, or "unset"
    name=${1##*/}
    h="$work/home $name"
    mkdir -p "$h"
    if [ "$1" = unset ]; then
        set -- env -i HOME="$h" PATH="$base_path" TMPDIR="$work"
    else
        set -- env -i HOME="$h" SHELL="$1" PATH="$base_path" TMPDIR="$work"
    fi

    printf '\n== SHELL=%s: fresh install ==\n' "$name"
    "$@" sh "$installer"
    check "$name fresh: binary runs" runs "$h/bin/ia-cli"
    if command -v zsh >/dev/null 2>&1; then
        check "$name fresh: a new login zsh finds ia-cli" finds_ia_cli "$h" zsh -l -c
        check "$name fresh: a new interactive zsh finds ia-cli" finds_ia_cli "$h" zsh -i -c
    fi
    check "$name fresh: a new login bash finds ia-cli" finds_ia_cli "$h" bash -l -c
    check "$name fresh: a new interactive non-login bash finds ia-cli" finds_ia_cli "$h" bash -i -c
    if command -v fish >/dev/null 2>&1; then
        check "$name fresh: a new login fish finds ia-cli" finds_ia_cli "$h" fish -l -c
    fi

    printf '\n== SHELL=%s: second run, piped to sh ==\n' "$name"
    "$@" sh <"$installer"
    check "$name rerun: binary runs" runs "$h/bin/ia-cli"
    for f in .zprofile .zshrc .profile .bashrc .config/fish/conf.d/ia-cli.fish; do
        if [ -e "$h/$f" ]; then
            check "$name rerun: one line in $f" one_line "$h/$f"
        fi
    done
}

run_case /bin/zsh
run_case /bin/bash
run_case /usr/bin/fish
run_case unset

printf '\n== pinned release: IA_VERSION=v0.27.0 ==\n'
h="$work/home pinned"
mkdir -p "$h"
env -i HOME="$h" SHELL=/bin/zsh PATH="$base_path" TMPDIR="$work" IA_VERSION=v0.27.0 sh "$installer"
check "pinned: installs 0.27.0" sh -c '[ "$("$1" --version)" = "ia-cli 0.27.0" ]' _ "$h/bin/ia-cli"

printf '\n%s failed\n' "$failures"
[ "$failures" -eq 0 ]
