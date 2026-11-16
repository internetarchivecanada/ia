#!/bin/sh
# shellcheck disable=SC2016 # sh -c scripts expand their own $1
# Offline tests for install.sh. No network: the "release" is a folder of
# fake files served by file://, and uname is replaced through the
# installer's test hooks (IA_INSTALL_BASE_URL, IA_INSTALL_UNAME_S,
# IA_INSTALL_UNAME_M).
#
# Usage: scripts/test-install.sh [SHELL...]
# Runs the installer under each shell given (default: sh, and dash if
# installed). Exits non-zero if any check fails.

set -eu

repo=$(cd "$(dirname "$0")/.." && pwd)
installer="$repo/install.sh"
work=$(mktemp -d "${TMPDIR:-/tmp}/ia-install-test.XXXXXX")
trap 'rm -rf "$work"' EXIT
trap 'exit 1' HUP INT TERM

failures=0
checks=0

pass() {
    checks=$((checks + 1))
    printf 'ok   %s\n' "$1"
}

fail() {
    checks=$((checks + 1))
    failures=$((failures + 1))
    printf 'FAIL %s\n' "$1"
    if [ -f "$work/out" ]; then
        sed 's/^/     | /' "$work/out"
    fi
}

check() { # description, command...
    desc=$1
    shift
    if "$@"; then pass "$desc"; else fail "$desc"; fi
}

sha256_of() {
    if command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{ print $1 }'
    else
        sha256sum "$1" | awk '{ print $1 }'
    fi
}

# make_release DIR VERSION [exit-status]: a fake release with a Linux asset
# that prints "ia-cli VERSION", and its SHA256SUMS.
make_release() {
    mkdir -p "$1"
    asset="$1/ia-x86_64-unknown-linux-musl"
    printf '#!/bin/sh\necho "ia-cli %s"\nexit %s\n' "$2" "${3:-0}" >"$asset"
    printf '%s  %s\n' "$(sha256_of "$asset")" ia-x86_64-unknown-linux-musl >"$1/SHA256SUMS"
    printf '%s  %s\n' 0000000000000000000000000000000000000000000000000000000000000000 \
        ia-aarch64-apple-darwin >>"$1/SHA256SUMS"
}

# run_installer HOME LOGIN-SHELL RELEASE-DIR [extra VAR=value...]
# Runs the installer with a clean environment; output goes to $work/out and
# the exit status to $status.
run_installer() {
    home=$1
    login_shell=$2
    release=$3
    shift 3
    status=0
    env -i HOME="$home" SHELL="$login_shell" PATH="/usr/bin:/bin:/usr/sbin:/sbin" \
        TMPDIR="$work" IA_INSTALL_BASE_URL="file://$release" \
        IA_INSTALL_UNAME_S=Linux IA_INSTALL_UNAME_M=x86_64 "$@" \
        "$sh" "$installer" >"$work/out" 2>&1 || status=$?
}

count_lines() { # file: lines added by the installer
    grep -c 'added by the ia-cli installer' "$1" 2>/dev/null || true
}

out_has() {
    grep -qF "$1" "$work/out"
}

no_leftovers() { # dir: no temporary files left beside the binary
    [ -z "$(find "$1" -name '.ia-cli.new.*' 2>/dev/null)" ] &&
        [ -z "$(find "$work" -maxdepth 1 -name 'ia-install.*' 2>/dev/null)" ]
}

run_suite() {
    sh=$1
    printf '\n== installer under %s ==\n' "$sh"
    rel1="$work/release-1.0"
    rel2="$work/release-2.0"
    make_release "$rel1" 1.0.0
    make_release "$rel2" 2.0.0

    # zsh: fresh install, then rerun, in a home folder with a space in it.
    h="$work/home $sh zsh"
    mkdir -p "$h"
    run_installer "$h" /bin/zsh "$rel1"
    check "zsh fresh: exits 0" [ "$status" -eq 0 ]
    check "zsh fresh: binary runs" sh -c '[ "$("$1" --version)" = "ia-cli 1.0.0" ]' _ "$h/bin/ia-cli"
    check "zsh fresh: mode 755" sh -c 'ls -l "$1" | grep -q "^-rwxr-xr-x"' _ "$h/bin/ia-cli"
    check "zsh fresh: one line in .zprofile" [ "$(count_lines "$h/.zprofile")" = 1 ]
    check "zsh fresh: says what to do next" out_has "log out and log back in, then type: ia-cli --version"
    check "zsh fresh: no temporary files left" no_leftovers "$h/bin"
    check "zsh fresh: profile line works with a space in HOME" \
        sh -c 'HOME="$1"; PATH=/usr/bin:/bin; . "$1/.zprofile"; [ "$(command -v ia-cli)" = "$1/bin/ia-cli" ]' _ "$h"

    run_installer "$h" /bin/zsh "$rel2"
    check "zsh rerun: exits 0" [ "$status" -eq 0 ]
    check "zsh rerun: updated" sh -c '[ "$("$1" --version)" = "ia-cli 2.0.0" ]' _ "$h/bin/ia-cli"
    check "zsh rerun: reports the update" out_has "Updated ia-cli 1.0.0 to ia-cli 2.0.0"
    check "zsh rerun: still one line in .zprofile" [ "$(count_lines "$h/.zprofile")" = 1 ]
    check "zsh rerun: says the profile was not changed" out_has "it was not changed"

    # zsh with ZDOTDIR.
    h="$work/home $sh zdotdir"
    mkdir -p "$h/zdot"
    run_installer "$h" /bin/zsh "$rel1" ZDOTDIR="$h/zdot"
    check "zsh ZDOTDIR: line in ZDOTDIR/.zprofile" [ "$(count_lines "$h/zdot/.zprofile")" = 1 ]
    check "zsh ZDOTDIR: no ~/.zprofile" [ ! -e "$h/.zprofile" ]

    # bash without .bash_profile: ~/.profile, appended on a line of its own.
    h="$work/home $sh bash-profile"
    mkdir -p "$h"
    printf 'existing line without newline' >"$h/.profile"
    run_installer "$h" /bin/bash "$rel1"
    run_installer "$h" /bin/bash "$rel1"
    check "bash, no .bash_profile: one line in .profile" [ "$(count_lines "$h/.profile")" = 1 ]
    check "bash, no .bash_profile: existing line kept" grep -qx 'existing line without newline' "$h/.profile"
    check "bash, no .bash_profile: no .bash_profile created" [ ! -e "$h/.bash_profile" ]
    check "bash, no .bash_profile: rerun says reinstalled" out_has "was already installed"

    # bash with .bash_profile.
    h="$work/home $sh bash-bash_profile"
    mkdir -p "$h"
    : >"$h/.bash_profile"
    run_installer "$h" /bin/bash "$rel1"
    run_installer "$h" /bin/bash "$rel1"
    check "bash with .bash_profile: one line there" [ "$(count_lines "$h/.bash_profile")" = 1 ]
    check "bash with .bash_profile: .profile untouched" [ ! -e "$h/.profile" ]

    # Folder already on PATH: no profile change.
    h="$work/home $sh onpath"
    mkdir -p "$h/bin"
    status=0
    env -i HOME="$h" SHELL=/bin/zsh PATH="$h/bin:/usr/bin:/bin" TMPDIR="$work" \
        IA_INSTALL_BASE_URL="file://$rel1" IA_INSTALL_UNAME_S=Linux IA_INSTALL_UNAME_M=x86_64 \
        "$sh" "$installer" >"$work/out" 2>&1 || status=$?
    check "on PATH: exits 0" [ "$status" -eq 0 ]
    check "on PATH: no profile written" [ ! -e "$h/.zprofile" ]
    check "on PATH: says ready" out_has "ia-cli is ready"

    # Other shell: line printed, no file written.
    h="$work/home $sh fish"
    mkdir -p "$h"
    run_installer "$h" /usr/local/bin/fish "$rel1"
    check "fish: exits 0" [ "$status" -eq 0 ]
    check "fish: prints the line" out_has 'export PATH="$HOME/bin:$PATH"'
    check "fish: no profile written" sh -c '[ -z "$(ls -A "$1" | grep -v "^bin$")" ]' _ "$h"

    # IA_INSTALL_DIR with a quote and a space.
    h="$work/home $sh custom"
    mkdir -p "$h"
    run_installer "$h" /bin/zsh "$rel1" IA_INSTALL_DIR="$h/it's here"
    check "IA_INSTALL_DIR: installed there" [ -x "$h/it's here/ia-cli" ]
    check "IA_INSTALL_DIR: profile line works" \
        sh -c 'PATH=/usr/bin:/bin; . "$1/.zprofile"; [ "$(command -v ia-cli)" = "$1/it'"'"'s here/ia-cli" ]' _ "$h"
    run_installer "$h" /bin/zsh "$rel1" IA_INSTALL_DIR="relative/dir"
    check "IA_INSTALL_DIR relative: refused" [ "$status" -ne 0 ]

    # Checksum mismatch: nothing installed, existing binary kept.
    bad="$work/release-bad"
    make_release "$bad" 9.9.9
    printf 'tampered\n' >>"$bad/ia-x86_64-unknown-linux-musl"
    h="$work/home $sh mismatch"
    mkdir -p "$h"
    run_installer "$h" /bin/zsh "$rel1"
    run_installer "$h" /bin/zsh "$bad"
    check "checksum mismatch: exits non-zero" [ "$status" -ne 0 ]
    check "checksum mismatch: says so" out_has "checksum mismatch"
    check "checksum mismatch: says not installed" out_has "ia-cli was not installed."
    check "checksum mismatch: old binary kept" sh -c '[ "$("$1" --version)" = "ia-cli 1.0.0" ]' _ "$h/bin/ia-cli"
    check "checksum mismatch: no temporary files left" no_leftovers "$h/bin"

    h="$work/home $sh mismatch-fresh"
    mkdir -p "$h"
    run_installer "$h" /bin/zsh "$bad"
    check "checksum mismatch, fresh: nothing in HOME" [ -z "$(ls -A "$h")" ]

    # Missing checksum line.
    miss="$work/release-missing"
    make_release "$miss" 1.0.0
    grep -v linux-musl "$miss/SHA256SUMS" >"$work/sums" && mv "$work/sums" "$miss/SHA256SUMS"
    h="$work/home $sh missing"
    mkdir -p "$h"
    run_installer "$h" /bin/zsh "$miss"
    check "missing checksum: exits non-zero" [ "$status" -ne 0 ]
    check "missing checksum: says so" out_has "has no checksum for ia-x86_64-unknown-linux-musl"
    check "missing checksum: nothing in HOME" [ -z "$(ls -A "$h")" ]

    # Download fails (no such release folder).
    h="$work/home $sh nofile"
    mkdir -p "$h"
    run_installer "$h" /bin/zsh "$work/no-such-release"
    check "download failure: exits non-zero" [ "$status" -ne 0 ]
    check "download failure: says so" out_has "The download failed."
    check "download failure: nothing in HOME" [ -z "$(ls -A "$h")" ]

    # A binary that cannot run does not replace one that can.
    broken="$work/release-broken"
    make_release "$broken" 3.0.0 1
    h="$work/home $sh broken"
    mkdir -p "$h"
    run_installer "$h" /bin/zsh "$rel1"
    run_installer "$h" /bin/zsh "$broken"
    check "binary cannot run: exits non-zero" [ "$status" -ne 0 ]
    check "binary cannot run: says so" out_has "could not run it"
    check "binary cannot run: old binary kept" sh -c '[ "$("$1" --version)" = "ia-cli 1.0.0" ]' _ "$h/bin/ia-cli"
    check "binary cannot run: no temporary files left" no_leftovers "$h/bin"

    h="$work/home $sh broken-fresh"
    mkdir -p "$h"
    run_installer "$h" /bin/zsh "$broken"
    check "binary cannot run, fresh: exits non-zero" [ "$status" -ne 0 ]
    check "binary cannot run, fresh: nothing in HOME" [ -z "$(ls -A "$h")" ]

    # Unsupported platforms.
    for platform in "FreeBSD amd64" "Linux aarch64" "Darwin i386"; do
        h="$work/home $sh unsupported"
        mkdir -p "$h"
        status=0
        env -i HOME="$h" SHELL=/bin/zsh PATH="/usr/bin:/bin" TMPDIR="$work" \
            IA_INSTALL_BASE_URL="file://$work/never-used" \
            IA_INSTALL_UNAME_S="${platform% *}" IA_INSTALL_UNAME_M="${platform#* }" \
            "$sh" "$installer" >"$work/out" 2>&1 || status=$?
        check "unsupported $platform: exits non-zero" [ "$status" -ne 0 ]
        check "unsupported $platform: names the platform" out_has "this computer ($platform)"
        check "unsupported $platform: points at releases" out_has "https://github.com/internetarchivecanada/ia/releases"
        check "unsupported $platform: nothing in HOME" [ -z "$(ls -A "$h")" ]
    done

    # Platform mapping (the Mac assets are fake 0000 checksums, so the run
    # stops at the checksum; the message names the asset it chose).
    for mapping in "Darwin arm64 ia-aarch64-apple-darwin" "Darwin x86_64 ia-x86_64-apple-darwin"; do
        # shellcheck disable=SC2086 # split into three words on purpose
        set -- $mapping
        h="$work/home $sh map"
        mkdir -p "$h"
        mapdir="$work/release-map"
        mkdir -p "$mapdir"
        printf 'x\n' >"$mapdir/$3"
        printf '%s  %s\n' "$(sha256_of "$mapdir/$3")" "$3" >"$mapdir/SHA256SUMS"
        printf '#!/bin/sh\necho "ia-cli mapped"\n' >"$mapdir/$3"
        status=0
        env -i HOME="$h" SHELL=/bin/zsh PATH="/usr/bin:/bin" TMPDIR="$work" \
            IA_INSTALL_BASE_URL="file://$mapdir" IA_INSTALL_UNAME_S="$1" IA_INSTALL_UNAME_M="$2" \
            "$sh" "$installer" >"$work/out" 2>&1 || status=$?
        check "$1 $2 picks $3" out_has "checksum mismatch"
        rm -rf "$mapdir"
    done
}

if [ "$#" -eq 0 ]; then
    set -- sh
    if command -v dash >/dev/null 2>&1; then
        set -- sh dash
    fi
fi
for shell in "$@"; do
    run_suite "$shell"
done

printf '\n%s checks, %s failed\n' "$checks" "$failures"
[ "$failures" -eq 0 ]
