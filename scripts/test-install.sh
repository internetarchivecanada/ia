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

# The installer sets PATH for zsh, bash and fish when it can find them, so
# its runs get a PATH that includes wherever they are installed here.
base_path=/usr/bin:/bin:/usr/sbin:/sbin
for s in zsh bash fish; do
    p=$(command -v "$s" 2>/dev/null || true)
    case ":$base_path:" in
        *":${p%/*}:"*) ;;
        *) if [ -n "$p" ]; then base_path="$base_path:${p%/*}"; fi ;;
    esac
done

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
    env -i HOME="$home" SHELL="$login_shell" PATH="$base_path" \
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

    # Whatever SHELL says (or if it is unset), one run sets PATH for zsh,
    # bash and fish; a new shell of each kind finds ia-cli; a rerun adds
    # nothing. The home folder has a space in its name.
    for login in /bin/zsh /bin/bash /usr/bin/fish unset; do
        name=${login##*/}
        h="$work/home $sh $name"
        mkdir -p "$h"
        if [ "$login" = unset ]; then
            status=0
            env -i HOME="$h" PATH="$base_path" TMPDIR="$work" IA_INSTALL_BASE_URL="file://$rel1" \
                IA_INSTALL_UNAME_S=Linux IA_INSTALL_UNAME_M=x86_64 \
                "$sh" "$installer" >"$work/out" 2>&1 || status=$?
        else
            run_installer "$h" "$login" "$rel1"
        fi
        check "SHELL=$name: exits 0" [ "$status" -eq 0 ]
        check "SHELL=$name: one instruction" out_has "Next: Open a new terminal window, then type: ia-cli --version"
        check "SHELL=$name: no line to copy" sh -c '! grep -q "export PATH\|set -gx" "$1"' _ "$work/out"
        check "SHELL=$name: mode 755" sh -c 'ls -l "$1" | grep -q "^-rwxr-xr-x"' _ "$h/bin/ia-cli"
        check "SHELL=$name: no temporary files left" no_leftovers "$h/bin"
        if command -v zsh >/dev/null 2>&1; then
            check "SHELL=$name: a new login zsh finds ia-cli" finds_ia_cli "$h" zsh -l -c
            check "SHELL=$name: a new interactive zsh finds ia-cli" finds_ia_cli "$h" zsh -i -c
        fi
        check "SHELL=$name: a new login bash finds ia-cli" finds_ia_cli "$h" bash -l -c
        check "SHELL=$name: a new interactive non-login bash finds ia-cli" finds_ia_cli "$h" bash -i -c
        check "SHELL=$name: a new sh reading ~/.profile finds ia-cli" \
            sh -c 'HOME="$1"; PATH=/usr/bin:/bin; . "$1/.profile"; [ "$(command -v ia-cli)" = "$1/bin/ia-cli" ]' _ "$h"
        if command -v fish >/dev/null 2>&1; then
            check "SHELL=$name: a new login fish finds ia-cli" finds_ia_cli "$h" fish -l -c
            check "SHELL=$name: a new non-login fish finds ia-cli" finds_ia_cli "$h" fish -c
        fi
        run_installer "$h" "$login" "$rel2"
        check "SHELL=$name rerun: updated" sh -c '[ "$("$1" --version)" = "ia-cli 2.0.0" ]' _ "$h/bin/ia-cli"
        check "SHELL=$name rerun: reports the update" out_has "Updated ia-cli 1.0.0 to ia-cli 2.0.0"
        check "SHELL=$name rerun: says already set up" out_has "(Already set up:"
        for f in .zprofile .zshrc .profile .bashrc .config/fish/conf.d/ia-cli.fish; do
            if [ -e "$h/$f" ]; then
                check "SHELL=$name rerun: one line in $f" [ "$(count_lines "$h/$f")" = 1 ]
            fi
        done
    done
    check "files: zsh, bash and sh files written" \
        sh -c 'for f in .zprofile .zshrc .profile .bashrc; do [ -f "$1/$f" ] || exit 1; done' _ "$work/home $sh zsh"
    if command -v fish >/dev/null 2>&1; then
        check "files: fish file written" [ -f "$work/home $sh zsh/.config/fish/conf.d/ia-cli.fish" ]
    fi
    check "files: no .bash_profile created" [ ! -e "$work/home $sh zsh/.bash_profile" ]
    if command -v bash >/dev/null 2>&1; then
        check "bash: PATH has the folder once when .profile and .bashrc are both read" \
            sh -c '[ "$(env -i HOME="$1" PATH=/usr/bin:/bin "$2" -c ". \"\$HOME/.profile\"; . \"\$HOME/.bashrc\"; echo \$PATH")" = "$1/bin:/usr/bin:/bin" ]' _ "$work/home $sh bash" "$(command -v bash)"
    fi
    if command -v fish >/dev/null 2>&1; then
        check "fish: PATH has the folder once when the file is read twice" \
            sh -c '[ "$(env -i HOME="$1" PATH=/usr/bin:/bin "$3" -c "source \"$2\"; string join \\n \$PATH" | grep -cx "$1/bin")" = 1 ]' _ "$work/home $sh fish" "$work/home $sh fish/.config/fish/conf.d/ia-cli.fish" "$(command -v fish)"
    fi

    # fish is neither installed nor configured: nothing written for it.
    h="$work/home $sh nofish"
    mkdir -p "$h"
    status=0
    env -i HOME="$h" SHELL=/bin/zsh PATH="/usr/bin:/bin:/usr/sbin:/sbin" TMPDIR="$work" \
        IA_INSTALL_BASE_URL="file://$rel1" IA_INSTALL_UNAME_S=Linux IA_INSTALL_UNAME_M=x86_64 \
        "$sh" "$installer" >"$work/out" 2>&1 || status=$?
    if ! [ -x /usr/bin/fish ] && ! [ -x /bin/fish ]; then
        check "no fish: no ~/.config created" [ ! -e "$h/.config" ]
    fi

    # fish config folder exists (fish not on PATH), with XDG_CONFIG_HOME and
    # another conf.d file: ia-cli.fish added beside it, the other untouched.
    h="$work/home $sh fish-xdg"
    mkdir -p "$h/xdg/fish/conf.d"
    printf 'set -g other 1\n' >"$h/xdg/fish/conf.d/other.fish"
    status=0
    env -i HOME="$h" SHELL=/bin/zsh PATH="/usr/bin:/bin:/usr/sbin:/sbin" TMPDIR="$work" XDG_CONFIG_HOME="$h/xdg" \
        IA_INSTALL_BASE_URL="file://$rel1" IA_INSTALL_UNAME_S=Linux IA_INSTALL_UNAME_M=x86_64 \
        "$sh" "$installer" >"$work/out" 2>&1 || status=$?
    check "fish config folder: line in XDG conf.d" [ "$(count_lines "$h/xdg/fish/conf.d/ia-cli.fish")" = 1 ]
    check "fish config folder: other file untouched" [ "$(cat "$h/xdg/fish/conf.d/other.fish")" = "set -g other 1" ]
    check "fish config folder: no ~/.config" [ ! -e "$h/.config" ]

    # zsh with an exported ZDOTDIR.
    h="$work/home $sh zdotdir"
    mkdir -p "$h/zdot"
    run_installer "$h" /bin/zsh "$rel1" ZDOTDIR="$h/zdot"
    check "ZDOTDIR: line in ZDOTDIR/.zprofile" [ "$(count_lines "$h/zdot/.zprofile")" = 1 ]
    check "ZDOTDIR: line in ZDOTDIR/.zshrc" [ "$(count_lines "$h/zdot/.zshrc")" = 1 ]
    check "ZDOTDIR: no ~/.zprofile" [ ! -e "$h/.zprofile" ]

    # Existing ~/.profile without a final newline: appended on a line of its own.
    h="$work/home $sh profile-nonl"
    mkdir -p "$h"
    printf '# existing line without newline' >"$h/.profile"
    run_installer "$h" /bin/bash "$rel1"
    check "existing .profile: first line kept" sh -c '[ "$(head -n 1 "$1")" = "# existing line without newline" ]' _ "$h/.profile"
    check "existing .profile: one line added" [ "$(count_lines "$h/.profile")" = 1 ]

    # bash with .bash_profile: the line goes there, not in ~/.profile.
    h="$work/home $sh bash-bash_profile"
    mkdir -p "$h"
    : >"$h/.bash_profile"
    run_installer "$h" /bin/bash "$rel1"
    check "with .bash_profile: one line there" [ "$(count_lines "$h/.bash_profile")" = 1 ]
    check "with .bash_profile: no .profile" [ ! -e "$h/.profile" ]
    check "with .bash_profile: a new login bash finds ia-cli" finds_ia_cli "$h" bash -l -c

    # A commented-out marked line does not count; a relative PATH entry that
    # happens to be the install folder does not count either.
    h="$work/home $sh commented"
    mkdir -p "$h/bin"
    printf '%s\n' '#export PATH="$HOME/bin:$PATH" # added by the ia-cli installer' >"$h/.zprofile"
    status=0
    (cd "$h/bin" && env -i HOME="$h" SHELL=/bin/zsh PATH=".:$base_path" TMPDIR="$work" \
        IA_INSTALL_BASE_URL="file://$rel1" IA_INSTALL_UNAME_S=Linux IA_INSTALL_UNAME_M=x86_64 \
        "$sh" "$installer") >"$work/out" 2>&1 || status=$?
    check "relative PATH entry: not taken as on PATH" out_has "Next: Open a new terminal window"
    check "commented-out line: a real line added" [ "$(count_lines "$h/.zprofile")" = 2 ]

    # Folder already on PATH: no file changed.
    h="$work/home $sh onpath"
    mkdir -p "$h/bin"
    status=0
    env -i HOME="$h" SHELL=/bin/zsh PATH="$h/bin:$base_path" TMPDIR="$work" \
        IA_INSTALL_BASE_URL="file://$rel1" IA_INSTALL_UNAME_S=Linux IA_INSTALL_UNAME_M=x86_64 \
        "$sh" "$installer" >"$work/out" 2>&1 || status=$?
    check "on PATH: exits 0" [ "$status" -eq 0 ]
    check "on PATH: says ready" out_has "ia-cli is ready. Type: ia-cli --version"
    check "on PATH: nothing but bin in HOME" sh -c '[ -z "$(ls -A "$1" | grep -v "^bin$")" ]' _ "$h"

    # Upgrade from the line the first version wrote to .zprofile.
    h="$work/home $sh oldline"
    mkdir -p "$h"
    printf '%s\n' 'export PATH="$HOME/bin:$PATH" # added by the ia-cli installer' >"$h/.zprofile"
    run_installer "$h" /bin/zsh "$rel1"
    check "earlier line: .zprofile not changed" [ "$(count_lines "$h/.zprofile")" = 1 ]
    check "earlier line: .zshrc gets one line" [ "$(count_lines "$h/.zshrc")" = 1 ]

    # No file can be written: the last-resort message with the line to add.
    h="$work/home $sh readonly-home"
    mkdir -p "$h" "$work/bin-$sh-ro"
    chmod 555 "$h"
    status=0
    env -i HOME="$h" SHELL=/bin/zsh PATH="/usr/bin:/bin:/usr/sbin:/sbin" TMPDIR="$work" \
        IA_INSTALL_DIR="$work/bin-$sh-ro" IA_INSTALL_BASE_URL="file://$rel1" \
        IA_INSTALL_UNAME_S=Linux IA_INSTALL_UNAME_M=x86_64 \
        "$sh" "$installer" >"$work/out" 2>&1 || status=$?
    chmod 755 "$h"
    check "nothing writable: exits 0" [ "$status" -eq 0 ]
    check "nothing writable: says so" out_has "new terminal windows will not find it yet"
    check "nothing writable: gives the line" out_has "    export PATH='$work/bin-$sh-ro':\"\$PATH\""

    # IA_INSTALL_DIR with a space, quotes, a $ and a backslash.
    h="$work/home $sh custom"
    mkdir -p "$h"
    cdir="$h/it's \"a\" \$b\\c"
    run_installer "$h" /bin/zsh "$rel1" IA_INSTALL_DIR="$cdir"
    check "IA_INSTALL_DIR: installed there" [ -x "$cdir/ia-cli" ]
    check "IA_INSTALL_DIR: sh line works" \
        sh -c 'PATH=/usr/bin:/bin; . "$1/.zprofile"; [ "$(command -v ia-cli)" = "$2/ia-cli" ]' _ "$h" "$cdir"
    for shell in bash zsh fish; do
        if command -v "$shell" >/dev/null 2>&1; then
            check "IA_INSTALL_DIR: a new $shell finds it" \
                sh -c '[ "$(env -i HOME="$1" PATH=/usr/bin:/bin TERM=dumb "$3" -i -c "command -v ia-cli" 2>/dev/null </dev/null | tail -n 1)" = "$2/ia-cli" ]' _ "$h" "$cdir" "$(command -v "$shell")"
        fi
    done
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
    check "Mac: same instruction" out_has "Next: Open a new terminal window, then type: ia-cli --version"

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
    check "profile not writable: listed quietly" out_has "(Could not change, no permission: ~/.zprofile)"
    check "profile not writable: still one instruction" out_has "Next: Open a new terminal window"
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
