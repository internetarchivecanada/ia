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
work=$(cd "$work" && pwd -P) # no // or symlinks, so shells print the same path
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

# make_named_release DIR ASSET VERSION [sums-prefix]: a release holding one
# fake asset; sums-prefix "*" writes the SHA256SUMS line in "*name" form.
make_named_release() {
    mkdir -p "$1"
    printf '#!/bin/sh\necho "ia-cli %s"\n' "$3" >"$1/$2"
    printf '%s  %s%s\n' "$(sha256_of "$1/$2")" "${4:-}" "$2" >"$1/SHA256SUMS"
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

# finds_ia_cli HOME SHELL-COMMAND...: a new shell started that way, with
# only HOME and a minimal PATH, finds HOME/bin/ia-cli.
finds_ia_cli() {
    fh=$1
    shell_path=$(command -v "$2")
    shift 2
    set -- "$shell_path" "$@"
    [ "$(env -i HOME="$fh" PATH=/usr/bin:/bin TERM=dumb "$@" 'command -v ia-cli' 2>/dev/null </dev/null | tail -n 1)" = "$fh/bin/ia-cli" ]
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
    check "zsh fresh: one line in .zshrc" [ "$(count_lines "$h/.zshrc")" = 1 ]
    check "zsh fresh (Linux): says a new terminal is enough" out_has "Next: open a new terminal window, then type: ia-cli --version"
    check "zsh fresh: prints a line for this window" out_has '    export PATH="$HOME/bin:$PATH"'
    if command -v zsh >/dev/null 2>&1; then
        check "zsh fresh: a new interactive zsh finds ia-cli" finds_ia_cli "$h" zsh -i -c
        check "zsh fresh: a new login zsh finds ia-cli" finds_ia_cli "$h" zsh -l -c
    fi
    check "zsh fresh: no temporary files left" no_leftovers "$h/bin"
    check "zsh fresh: profile line works with a space in HOME" \
        sh -c 'HOME="$1"; PATH=/usr/bin:/bin; . "$1/.zprofile"; [ "$(command -v ia-cli)" = "$1/bin/ia-cli" ]' _ "$h"

    run_installer "$h" /bin/zsh "$rel2"
    check "zsh rerun: exits 0" [ "$status" -eq 0 ]
    check "zsh rerun: updated" sh -c '[ "$("$1" --version)" = "ia-cli 2.0.0" ]' _ "$h/bin/ia-cli"
    check "zsh rerun: reports the update" out_has "Updated ia-cli 1.0.0 to ia-cli 2.0.0"
    check "zsh rerun: still one line in .zprofile" [ "$(count_lines "$h/.zprofile")" = 1 ]
    check "zsh rerun: still one line in .zshrc" [ "$(count_lines "$h/.zshrc")" = 1 ]
    check "zsh rerun: says the files were not changed" out_has "Already set up, not changed:"

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
    check "bash, no .bash_profile: one line in .bashrc" [ "$(count_lines "$h/.bashrc")" = 1 ]
    check "bash (Linux): says a new terminal is enough" out_has "Next: open a new terminal window"
    if command -v bash >/dev/null 2>&1; then
        check "bash: a new interactive non-login bash finds ia-cli" finds_ia_cli "$h" bash -i -c
        check "bash: a new login bash finds ia-cli" finds_ia_cli "$h" bash -l -c
        check "bash: PATH has the folder once with both files read" \
            sh -c '[ "$(env -i HOME="$1" PATH=/usr/bin:/bin "$2" -c ". \"\$HOME/.profile\"; . \"\$HOME/.bashrc\"; echo \$PATH")" = "$1/bin:/usr/bin:/bin" ]' _ "$h" "$(command -v bash)"
    fi

    # bash with .bash_profile.
    h="$work/home $sh bash-bash_profile"
    mkdir -p "$h"
    : >"$h/.bash_profile"
    run_installer "$h" /bin/bash "$rel1"
    run_installer "$h" /bin/bash "$rel1"
    check "bash with .bash_profile: one line there" [ "$(count_lines "$h/.bash_profile")" = 1 ]
    check "bash with .bash_profile: .profile untouched" [ ! -e "$h/.profile" ]
    check "bash with .bash_profile: one line in .bashrc" [ "$(count_lines "$h/.bashrc")" = 1 ]

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

    # fish: conf.d file, created with its folder; rerun keeps one line.
    h="$work/home $sh fish"
    mkdir -p "$h"
    run_installer "$h" /usr/local/bin/fish "$rel1"
    run_installer "$h" /usr/local/bin/fish "$rel1"
    fishconf="$h/.config/fish/conf.d/ia-cli.fish"
    check "fish: exits 0" [ "$status" -eq 0 ]
    check "fish: one line in conf.d/ia-cli.fish" [ "$(count_lines "$fishconf")" = 1 ]
    check "fish: only that file written" sh -c '[ "$(cd "$1" && find . -type f ! -path "./bin/*")" = "./.config/fish/conf.d/ia-cli.fish" ]' _ "$h"
    check "fish: prints a fish line for this window" out_has '    set -gx PATH "$HOME/bin" $PATH'
    check "fish: prints no sh syntax" sh -c '! grep -q "export PATH" "$1"' _ "$work/out"
    if command -v fish >/dev/null 2>&1; then
        check "fish: a new login fish finds ia-cli" finds_ia_cli "$h" fish -l -c
        check "fish: a new non-login fish finds ia-cli" finds_ia_cli "$h" fish -c
        check "fish: PATH has the folder once when the file is read twice" \
            sh -c '[ "$(env -i HOME="$1" PATH=/usr/bin:/bin "$3" -c "source \"$2\"; string join \n \$PATH" | grep -cx "$1/bin")" = 1 ]' _ "$h" "$fishconf" "$(command -v fish)"
    fi

    # fish with an install folder whose name has a quote and a space.
    if command -v fish >/dev/null 2>&1; then
        h="$work/home $sh fish-custom"
        mkdir -p "$h"
        run_installer "$h" /usr/bin/fish "$rel1" IA_INSTALL_DIR="$h/it's here"
        check "fish custom folder: a new fish finds ia-cli" \
            sh -c '[ "$(env -i HOME="$1" PATH=/usr/bin:/bin "$2" -c "command -v ia-cli")" = "$1/it'"'"'s here/ia-cli" ]' _ "$h" "$(command -v fish)"
    fi

    # fish with XDG_CONFIG_HOME and another conf.d file already there.
    h="$work/home $sh fish-xdg"
    mkdir -p "$h/xdg/fish/conf.d"
    printf 'set -g other 1\n' >"$h/xdg/fish/conf.d/other.fish"
    run_installer "$h" /usr/bin/fish "$rel1" XDG_CONFIG_HOME="$h/xdg"
    check "fish XDG_CONFIG_HOME: line in XDG conf.d" [ "$(count_lines "$h/xdg/fish/conf.d/ia-cli.fish")" = 1 ]
    check "fish XDG_CONFIG_HOME: other file untouched" [ "$(cat "$h/xdg/fish/conf.d/other.fish")" = "set -g other 1" ]
    check "fish XDG_CONFIG_HOME: no ~/.config" [ ! -e "$h/.config" ]

    # tcsh: nothing written; the tcsh line and its file are printed.
    h="$work/home $sh tcsh"
    mkdir -p "$h"
    run_installer "$h" /bin/tcsh "$rel1"
    check "tcsh: exits 0" [ "$status" -eq 0 ]
    check "tcsh: prints the setenv line" out_has 'setenv PATH "$HOME/bin:$PATH" # added by the ia-cli installer'
    check "tcsh: names .tcshrc" out_has "Add this line to $h/.tcshrc"
    check "tcsh: prints no sh syntax" sh -c '! grep -q "export PATH" "$1"' _ "$work/out"
    check "tcsh: no file written" sh -c '[ -z "$(ls -A "$1" | grep -v "^bin$")" ]' _ "$h"
    if command -v tcsh >/dev/null 2>&1; then
        grep 'setenv PATH' "$work/out" | head -n 1 | sed 's/^ *//' >"$h/.tcshrc"
        check "tcsh: the printed line works in tcsh" \
            sh -c '[ "$(env -i HOME="$1" PATH=/usr/bin:/bin "$2" -c "which ia-cli")" = "$1/bin/ia-cli" ]' _ "$h" "$(command -v tcsh)"
    fi

    # Nushell and an unknown shell: printed, in the right syntax.
    h="$work/home $sh nu"
    mkdir -p "$h"
    run_installer "$h" /usr/bin/nu "$rel1"
    check "nu: prints the Nushell line" out_has "\$env.PATH = (\$env.PATH | prepend \"$h/bin\")"
    check "nu: names config.nu" out_has "config.nu"
    h="$work/home $sh unknown"
    mkdir -p "$h"
    run_installer "$h" "" "$rel1"
    check "no SHELL: says which shells the line is for" out_has "this line is for sh, bash, zsh and ksh"
    check "no SHELL: no file written" sh -c '[ -z "$(ls -A "$1" | grep -v "^bin$")" ]' _ "$h"

    # sh as the login shell: ~/.profile, read only at login.
    h="$work/home $sh sh"
    mkdir -p "$h"
    run_installer "$h" /bin/sh "$rel1"
    check "sh: one line in .profile" [ "$(count_lines "$h/.profile")" = 1 ]
    check "sh (Linux): says to log out and back in" out_has "Next: log out and log back in"

    # Upgrade from the line earlier versions wrote to .zprofile.
    h="$work/home $sh oldline"
    mkdir -p "$h"
    printf '%s\n' 'export PATH="$HOME/bin:$PATH" # added by the ia-cli installer' >"$h/.zprofile"
    run_installer "$h" /bin/zsh "$rel1"
    check "earlier line: .zprofile not changed" [ "$(count_lines "$h/.zprofile")" = 1 ]
    check "earlier line: .zshrc gets one line" [ "$(count_lines "$h/.zshrc")" = 1 ]

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
    check "download failure: says so" out_has "The download failed: could not reach GitHub."
    check "download failure: no raw curl lines" sh -c '! grep -q "^curl:" "$1"' _ "$work/out"
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

    # Mac success messages.
    mac="$work/release-mac"
    make_named_release "$mac" ia-aarch64-apple-darwin 1.0.0
    h="$work/home $sh mac"
    mkdir -p "$h"
    run_installer "$h" /bin/zsh "$mac" IA_INSTALL_UNAME_S=Darwin IA_INSTALL_UNAME_M=arm64
    check "Mac: exits 0" [ "$status" -eq 0 ]
    check "Mac: says to open a new Terminal window" out_has "Next: open a new Terminal window, then type: ia-cli --version"

    # SHA256SUMS line in "*name" (binary mode) form.
    star="$work/release-star"
    make_named_release "$star" ia-x86_64-unknown-linux-musl 1.0.0 "*"
    h="$work/home $sh star"
    mkdir -p "$h"
    run_installer "$h" /bin/zsh "$star"
    check "*name checksum line: installs" [ -x "$h/bin/ia-cli" ]

    # bash with only .bash_login.
    h="$work/home $sh bash_login"
    mkdir -p "$h"
    : >"$h/.bash_login"
    run_installer "$h" /bin/bash "$rel1"
    check "bash with .bash_login: one line there" [ "$(count_lines "$h/.bash_login")" = 1 ]
    check "bash with .bash_login: no .profile" [ ! -e "$h/.profile" ]

    # Profile that cannot be written: binary installed, line printed.
    h="$work/home $sh readonly-profile"
    mkdir -p "$h"
    : >"$h/.zprofile"
    chmod 000 "$h/.zprofile"
    run_installer "$h" /bin/zsh "$rel1"
    chmod 644 "$h/.zprofile"
    check "profile not writable: exits 0" [ "$status" -eq 0 ]
    check "profile not writable: says so" out_has "Could not change (no permission), left as they were:"
    check "profile not writable: other file still set" [ "$(count_lines "$h/.zshrc")" = 1 ]
    check "profile not writable: no raw errors" sh -c '! grep -q "Permission denied" "$1"' _ "$work/out"
    check "profile not writable: profile unchanged" [ ! -s "$h/.zprofile" ]

    # HOME with a trailing slash: same line, written once.
    h="$work/home $sh slash"
    mkdir -p "$h"
    run_installer "$h/" /bin/zsh "$rel1"
    run_installer "$h/" /bin/zsh "$rel1"
    check "HOME with trailing slash: installed" [ -x "$h/bin/ia-cli" ]
    check "HOME with trailing slash: one \$HOME line" grep -qxF 'case ":$PATH:" in *:"$HOME/bin":*) ;; *) export PATH="$HOME/bin:$PATH" ;; esac # added by the ia-cli installer' "$h/.zprofile"
    check "HOME with trailing slash: still one line" [ "$(count_lines "$h/.zprofile")" = 1 ]

    # Install folder already on PATH, spelled with a trailing slash: ready,
    # and no note about "another" ia-cli.
    h="$work/home $sh path-slash"
    mkdir -p "$h/bin"
    status=0
    env -i HOME="$h" SHELL=/bin/zsh PATH="$h/bin/:/usr/bin:/bin" TMPDIR="$work" \
        IA_INSTALL_BASE_URL="file://$rel1" IA_INSTALL_UNAME_S=Linux IA_INSTALL_UNAME_M=x86_64 \
        "$sh" "$installer" >"$work/out" 2>&1 || status=$?
    check "PATH with trailing slash: says ready" out_has "ia-cli is ready"
    check "PATH with trailing slash: no note about another copy" sh -c '! grep -q "another ia-cli" "$1"' _ "$work/out"

    # Another ia-cli earlier on PATH than the install folder: noted.
    other="$work/other-$sh"
    mkdir -p "$other"
    printf '#!/bin/sh\necho "ia-cli 0.1.0"\n' >"$other/ia-cli"
    chmod 755 "$other/ia-cli"
    h="$work/home $sh other-first"
    mkdir -p "$h/bin"
    status=0
    env -i HOME="$h" SHELL=/bin/zsh PATH="$other:$h/bin:/usr/bin:/bin" TMPDIR="$work" \
        IA_INSTALL_BASE_URL="file://$rel1" IA_INSTALL_UNAME_S=Linux IA_INSTALL_UNAME_M=x86_64 \
        "$sh" "$installer" >"$work/out" 2>&1 || status=$?
    check "other copy first on PATH: noted" out_has "another ia-cli is at $other/ia-cli"

    # Another ia-cli on PATH, install folder not on PATH: the new profile
    # line puts the install folder first, so no note.
    h="$work/home $sh other-added"
    mkdir -p "$h"
    status=0
    env -i HOME="$h" SHELL=/bin/zsh PATH="$other:/usr/bin:/bin" TMPDIR="$work" \
        IA_INSTALL_BASE_URL="file://$rel1" IA_INSTALL_UNAME_S=Linux IA_INSTALL_UNAME_M=x86_64 \
        "$sh" "$installer" >"$work/out" 2>&1 || status=$?
    check "other copy, line added: no note" sh -c '! grep -q "another ia-cli" "$1"' _ "$work/out"

    # The destination is a folder.
    h="$work/home $sh dest-dir"
    mkdir -p "$h/bin/ia-cli"
    run_installer "$h" /bin/zsh "$rel1"
    check "destination is a folder: exits non-zero" [ "$status" -ne 0 ]
    check "destination is a folder: says so" out_has "is a folder"

    # Install folder that cannot be written to.
    h="$work/home $sh ro-dir"
    mkdir -p "$h/bin"
    chmod 555 "$h/bin"
    run_installer "$h" /bin/zsh "$rel1"
    chmod 755 "$h/bin"
    check "install folder not writable: exits non-zero" [ "$status" -ne 0 ]
    check "install folder not writable: says so" out_has "Cannot write to the folder $h/bin"
    check "install folder not writable: no profile written" [ ! -e "$h/.zprofile" ]

    # Neither zsh file writable: the line is printed.
    h="$work/home $sh readonly-both"
    mkdir -p "$h"
    : >"$h/.zprofile"
    : >"$h/.zshrc"
    chmod 000 "$h/.zprofile" "$h/.zshrc"
    run_installer "$h" /bin/zsh "$rel1"
    chmod 644 "$h/.zprofile" "$h/.zshrc"
    check "no profile writable: exits 0" [ "$status" -eq 0 ]
    check "no profile writable: says so" out_has "Could not change $h/.zprofile (no permission)"
    check "no profile writable: prints the line" out_has '*) export PATH="$HOME/bin:$PATH" ;; esac # added by the ia-cli installer'

    # ~/bin is a file.
    h="$work/home $sh bin-file"
    mkdir -p "$h"
    : >"$h/bin"
    run_installer "$h" /bin/zsh "$rel1"
    check "bin is a file: exits non-zero" [ "$status" -ne 0 ]
    check "bin is a file: says so" out_has "$h/bin is a file, not a folder"

    # TMPDIR that does not exist: falls back to /tmp.
    h="$work/home $sh bad-tmpdir"
    mkdir -p "$h"
    status=0
    env -i HOME="$h" SHELL=/bin/zsh PATH="/usr/bin:/bin" TMPDIR="$work/no-such-tmp" \
        IA_INSTALL_BASE_URL="file://$rel1" IA_INSTALL_UNAME_S=Linux IA_INSTALL_UNAME_M=x86_64 \
        "$sh" "$installer" >"$work/out" 2>&1 || status=$?
    check "TMPDIR missing: installs" [ -x "$h/bin/ia-cli" ]
    check "TMPDIR missing: no raw mktemp error" sh -c '! grep -q "mktemp" "$1"' _ "$work/out"

    # HOME not set.
    status=0
    env -i SHELL=/bin/zsh PATH="/usr/bin:/bin" "$sh" "$installer" >"$work/out" 2>&1 || status=$?
    check "HOME unset: exits non-zero" [ "$status" -ne 0 ]
    check "HOME unset: says so" out_has "The HOME variable is not set"

    # IA_VERSION that is not a release name (checked before any download).
    h="$work/home $sh badversion"
    mkdir -p "$h"
    status=0
    env -i HOME="$h" SHELL=/bin/zsh PATH="/usr/bin:/bin" TMPDIR="$work" \
        IA_VERSION='v1;touch x' IA_INSTALL_UNAME_S=Linux IA_INSTALL_UNAME_M=x86_64 \
        "$sh" "$installer" >"$work/out" 2>&1 || status=$?
    check "bad IA_VERSION: exits non-zero" [ "$status" -ne 0 ]
    check "bad IA_VERSION: says so" out_has "IA_VERSION is not a release name"
    check "bad IA_VERSION: nothing in HOME" [ -z "$(ls -A "$h")" ]

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
