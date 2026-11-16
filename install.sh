#!/bin/sh
# Install or update ia-cli, the Internet Archive command-line tool, on macOS
# or Linux:
#
#   curl -fsSL https://raw.githubusercontent.com/internetarchivecanada/ia/main/install.sh | sh
#
# What it does:
#   1. Picks the release binary for this computer (uname; under Rosetta the
#      Apple Silicon build).
#   2. Downloads it and SHA256SUMS from the GitHub release, and stops without
#      installing anything if the checksum does not match.
#   3. Copies it to $HOME/bin/ia-cli, runs it with --version, and only then
#      replaces any existing ia-cli there.
#   4. If that folder is not on PATH, adds one line, ending in
#      "# added by the ia-cli installer", to ~/.zprofile (zsh) or to
#      ~/.bash_profile, ~/.bash_login or ~/.profile (bash). For other shells
#      it prints the line instead.
# It never uses sudo and writes nothing outside $HOME unless IA_INSTALL_DIR
# says so. Running it again updates to the latest release.
#
# Uninstall: delete the binary and the one line it added to the profile.
#
# Settings (environment variables):
#   IA_VERSION       release to install, for example v0.27.0 (default: latest)
#   IA_INSTALL_DIR   folder to install into, a full path (default: $HOME/bin)
#
# Test hooks, used by scripts/test-install.sh; not for normal use:
#   IA_INSTALL_BASE_URL  download from this URL (file:// allowed) instead of
#                        the GitHub release
#   IA_INSTALL_UNAME_S   used instead of `uname -s`
#   IA_INSTALL_UNAME_M   used instead of `uname -m` (also skips the Rosetta check)
#
# The whole script is functions; main runs on the last line, so a download
# cut short runs nothing.

set -eu

RELEASES_URL="https://github.com/internetarchivecanada/ia/releases"
PROFILE_MARK="# added by the ia-cli installer"

tmp_dir=""
new_bin=""
made_dir=""
installed=""

say() {
    printf '%s\n' "$*"
}

setup_colours() {
    bold=""
    reset=""
    err_red=""
    err_reset=""
    if [ -z "${NO_COLOR:-}" ] && [ "${TERM:-dumb}" != dumb ]; then
        if [ -t 1 ]; then
            bold=$(printf '\033[1m')
            reset=$(printf '\033[0m')
        fi
        if [ -t 2 ]; then
            err_red=$(printf '\033[1;31m')
            err_reset=$(printf '\033[0m')
        fi
    fi
}

# Print the reason on stderr, one line per argument, and stop.
die() {
    printf '%s\n' "${err_red}ia-cli was not installed.${err_reset}" >&2
    for line in "$@"; do
        printf '%s\n' "$line" >&2
    done
    exit 1
}

cleanup() {
    if [ -n "$new_bin" ]; then
        rm -f "$new_bin"
    fi
    if [ -n "$tmp_dir" ]; then
        rm -rf "$tmp_dir"
    fi
    # A folder this run created for a binary it did not install: remove it
    # again if it is still empty (rmdir removes only empty folders).
    if [ -n "$made_dir" ] && [ -z "$installed" ]; then
        rmdir "$made_dir" 2>/dev/null || true
    fi
}

has() {
    command -v "$1" >/dev/null 2>&1
}

# Sets asset, label and min_macos from the operating system and processor.
detect_asset() {
    os=${IA_INSTALL_UNAME_S:-$(uname -s)}
    arch=${IA_INSTALL_UNAME_M:-$(uname -m)}
    # A shell running under Rosetta reports x86_64 on an Apple Silicon Mac;
    # the native build is the right one there.
    if [ "$os" = Darwin ] && [ "$arch" = x86_64 ] && [ -z "${IA_INSTALL_UNAME_M:-}" ]; then
        if [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" = 1 ]; then
            arch=arm64
        fi
    fi
    case "$os/$arch" in
        Darwin/arm64 | Darwin/aarch64)
            asset=ia-aarch64-apple-darwin
            label="Mac with Apple Silicon"
            min_macos=11
            ;;
        Darwin/x86_64)
            asset=ia-x86_64-apple-darwin
            label="Mac with an Intel processor"
            min_macos=10.14
            ;;
        Linux/x86_64 | Linux/amd64)
            asset=ia-x86_64-unknown-linux-musl
            label="Linux (x86_64)"
            min_macos=""
            ;;
        *)
            die "There is no ia-cli download for this computer ($os $arch)." \
                "The downloads that exist are listed at $RELEASES_URL"
            ;;
    esac
}

# Sets base_url, the folder the asset and SHA256SUMS are downloaded from.
choose_base_url() {
    if [ -n "${IA_INSTALL_BASE_URL:-}" ]; then
        base_url=${IA_INSTALL_BASE_URL%/}
        say "Test setting IA_INSTALL_BASE_URL is set: downloading from $base_url"
    elif [ -n "${IA_VERSION:-}" ]; then
        case $IA_VERSION in
            v*) tag=$IA_VERSION ;;
            *) tag=v$IA_VERSION ;;
        esac
        case $tag in
            *[!A-Za-z0-9._-]*)
                die "IA_VERSION is not a release name: $IA_VERSION" \
                    "Release names look like v0.27.0; they are listed at $RELEASES_URL"
                ;;
        esac
        base_url="$RELEASES_URL/download/$tag"
    else
        base_url="$RELEASES_URL/latest/download"
    fi
}

# download URL FILE
download() {
    if has curl; then
        if [ -n "${IA_INSTALL_BASE_URL:-}" ]; then
            curl -fsSL --retry 3 -o "$2" "$1"
        else
            curl --proto '=https' --tlsv1.2 -fsSL --retry 3 -o "$2" "$1"
        fi
    else
        wget -q -O "$2" "$1"
    fi
}

# sha256 FILE: print the file's SHA-256 in hex. need_tools checked that one
# of the two programs exists.
sha256() {
    if has sha256sum; then
        sha256sum "$1" | awk '{ print $1 }'
    else
        shasum -a 256 "$1" | awk '{ print $1 }'
    fi
}

need_tools() {
    if ! has curl && ! has wget; then
        die "This computer has neither curl nor wget, and one of them is needed to download ia-cli."
    fi
    if ! has shasum && ! has sha256sum; then
        die "This computer has neither shasum nor sha256sum, and one of them is needed to check the download."
    fi
}

download_and_verify() {
    say "Downloading ia-cli for $label..."
    if ! download "$base_url/SHA256SUMS" "$tmp_dir/SHA256SUMS" ||
        ! download "$base_url/$asset" "$tmp_dir/$asset"; then
        if [ -n "${IA_VERSION:-}" ]; then
            die "The download failed. Check that $IA_VERSION is a release listed at $RELEASES_URL"
        fi
        die "The download failed. Check the internet connection, then run the same command again."
    fi

    expected=$(awk -v a="$asset" '$2 == a || $2 == "*" a { print $1; exit }' "$tmp_dir/SHA256SUMS")
    case $expected in
        "" | *[!0-9a-f]*)
            die "The release has no checksum for $asset, so the download cannot be checked." \
                "Report this at https://github.com/internetarchivecanada/ia/issues"
            ;;
    esac
    if [ "${#expected}" -ne 64 ]; then
        die "The release has no valid checksum for $asset, so the download cannot be checked." \
            "Report this at https://github.com/internetarchivecanada/ia/issues"
    fi
    actual=$(sha256 "$tmp_dir/$asset" 2>/dev/null)
    if [ -z "$actual" ]; then
        die "This computer could not compute the download's checksum, so it cannot be checked." \
            "Report this at https://github.com/internetarchivecanada/ia/issues"
    fi
    if [ "$actual" != "$expected" ]; then
        die "The downloaded file is not the file the release lists (checksum mismatch)." \
            "Nothing was changed. Run the same command again; if this happens again, report it at" \
            "https://github.com/internetarchivecanada/ia/issues"
    fi
}

# Sets install_dir and dest.
choose_install_dir() {
    install_dir=${IA_INSTALL_DIR:-$home/bin}
    case $install_dir in
        /*) ;;
        *) die "IA_INSTALL_DIR must be a full path starting with /, not: $install_dir" ;;
    esac
    if [ "$install_dir" != / ]; then
        install_dir=${install_dir%/}
    fi
    dest="$install_dir/ia-cli"
}

# Copy the verified binary beside the destination, check it runs, then
# replace the destination in one rename.
install_binary() {
    if [ ! -d "$install_dir" ]; then
        made_dir=$install_dir
    fi
    if ! mkdir -p "$install_dir" 2>/dev/null || [ ! -w "$install_dir" ]; then
        die "Cannot write to the folder $install_dir" \
            "Run the command again with a folder you own, for example:" \
            "  curl -fsSL https://raw.githubusercontent.com/internetarchivecanada/ia/main/install.sh | IA_INSTALL_DIR=\"\$HOME/.local/bin\" sh"
    fi
    if [ -d "$dest" ]; then
        die "$dest is a folder, so ia-cli cannot be installed there." \
            "Move or rename that folder, then run the same command again."
    fi

    old_version=""
    if [ -f "$dest" ] && [ -x "$dest" ]; then
        old_version=$("$dest" --version </dev/null 2>/dev/null || true)
        old_version=$(printf '%s\n' "$old_version" | head -n 1)
    fi

    new_bin=$(mktemp "$install_dir/.ia-cli.new.XXXXXX") ||
        die "Cannot write to the folder $install_dir"
    if ! cp "$tmp_dir/$asset" "$new_bin" || ! chmod 755 "$new_bin"; then
        die "Cannot write ia-cli to the folder $install_dir" \
            "Check that the disk is not full, then run the same command again."
    fi

    if ! new_version=$("$new_bin" --version </dev/null 2>"$tmp_dir/run-error") ||
        [ -z "$new_version" ]; then
        details=$(head -n 3 "$tmp_dir/run-error")
        if [ -n "$min_macos" ]; then
            macos=$(sw_vers -productVersion 2>/dev/null || echo unknown)
            if version_older "$macos" "$min_macos"; then
                die "ia-cli was downloaded, but this Mac could not run it." \
                    "This Mac has macOS $macos. ia-cli needs macOS $min_macos or later." \
                    "Any ia-cli already installed was left as it was." \
                    "Details: $details"
            fi
            die "ia-cli was downloaded, but this Mac (macOS $macos, $(uname -m)) could not run it." \
                "Any ia-cli already installed was left as it was." \
                "Report this, with the details below, at https://github.com/internetarchivecanada/ia/issues" \
                "Details: $details"
        fi
        die "ia-cli was downloaded, but this computer ($(uname -sr)) could not run it." \
            "Any ia-cli already installed was left as it was." \
            "Report this, with the details below, at https://github.com/internetarchivecanada/ia/issues" \
            "Details: $details"
    fi

    new_version=$(printf '%s\n' "$new_version" | head -n 1)
    mv -f "$new_bin" "$dest" ||
        die "Cannot replace $dest" \
            "Check that the disk is not full, then run the same command again."
    new_bin=""
    installed=1
}

# version_older A B: true if version A (like 10.13.6) is older than B.
version_older() {
    awk -v a="$1" -v b="$2" 'BEGIN {
        na = split(a, x, "."); nb = split(b, y, ".")
        n = na > nb ? na : nb
        for (i = 1; i <= n; i++) {
            if (x[i] + 0 < y[i] + 0) exit 0
            if (x[i] + 0 > y[i] + 0) exit 1
        }
        exit 1
    }'
}

# real_dir DIR: the folder's path with symlinks and repeated slashes resolved.
real_dir() {
    (cd "$1" 2>/dev/null && pwd -P) || printf '%s\n' "$1"
}

on_path() {
    case ":${PATH:-}:" in
        *":$install_dir:"* | *":$install_dir/:"*) return 0 ;;
    esac
    # A PATH entry spelled differently (repeated slashes, a symlink) that is
    # the same folder.
    want=$(real_dir "$install_dir")
    found=1
    old_ifs=$IFS
    IFS=:
    set -f # split PATH on ":" without expanding * or ? in it
    for entry in ${PATH:-}; do
        if [ -n "$entry" ] && [ -d "$entry" ] && [ "$(real_dir "$entry")" = "$want" ]; then
            found=0
            break
        fi
    done
    set +f
    IFS=$old_ifs
    return "$found"
}

# Sets path_line, the line that puts install_dir on PATH.
make_path_line() {
    if [ "$install_dir" = "$home/bin" ]; then
        # shellcheck disable=SC2016 # $HOME and $PATH expand when the profile runs
        path_line='export PATH="$HOME/bin:$PATH" '"$PROFILE_MARK"
    else
        quoted=$(printf '%s' "$install_dir" | sed "s/'/'\\\\''/g")
        # shellcheck disable=SC2016 # $PATH expands when the profile runs
        path_line="export PATH='$quoted'"':"$PATH" '"$PROFILE_MARK"
    fi
}

# Sets profile to the login profile for the user's shell, or "" if unknown.
choose_profile() {
    shell_name=${SHELL:-}
    shell_name=${shell_name##*/}
    case $shell_name in
        zsh)
            # ZDOTDIR counts only when exported; one set inside ~/.zshenv
            # without export is not seen here, and ~/.zprofile is used.
            profile="${ZDOTDIR:-$home}/.zprofile"
            ;;
        bash)
            # bash reads only the first of these that exists.
            if [ -f "$home/.bash_profile" ]; then
                profile="$home/.bash_profile"
            elif [ -f "$home/.bash_login" ]; then
                profile="$home/.bash_login"
            else
                profile="$home/.profile"
            fi
            ;;
        *)
            profile=""
            ;;
    esac
}

# Append path_line to profile, on a line of its own.
append_path_line() {
    if [ -e "$profile" ] && { [ ! -r "$profile" ] || [ ! -w "$profile" ]; }; then
        return 1
    fi
    separator=""
    if [ -s "$profile" ] && [ -n "$(tail -c 1 "$profile" 2>/dev/null)" ]; then
        separator="
"
    fi
    printf '%s%s\n' "$separator" "$path_line" 2>/dev/null >>"$profile"
}

# Sets path_status: ready, added, present, manual or failed.
update_path() {
    if on_path; then
        path_status=ready
        return 0
    fi
    make_path_line
    choose_profile
    if [ -z "$profile" ]; then
        path_status=manual
    elif [ -f "$profile" ] && grep -qxF "$path_line" "$profile" 2>/dev/null; then
        path_status=present
    elif append_path_line; then
        path_status=added
    else
        path_status=failed
    fi
}

report() {
    say ""
    if [ -z "$old_version" ]; then
        say "${bold}Installed $new_version${reset} at $dest"
    elif [ "$old_version" = "$new_version" ]; then
        say "${bold}$new_version${reset} was already installed at $dest; it was checked and reinstalled."
    else
        say "${bold}Updated $old_version to $new_version${reset} at $dest"
    fi

    if [ "$os" = Darwin ]; then
        new_window="open a new Terminal window"
    else
        new_window="log out and log back in"
    fi

    case $path_status in
        ready)
            say ""
            say "ia-cli is ready. Type: ia-cli --version"
            ;;
        added)
            say "Added this line to $profile so the shell can find ia-cli:"
            say "    $path_line"
            say ""
            say "${bold}Next:${reset} $new_window, then type: ia-cli --version"
            ;;
        present)
            say "$profile already has the line that lets the shell find ia-cli; it was not changed."
            say ""
            say "${bold}Next:${reset} $new_window, then type: ia-cli --version"
            ;;
        manual | failed)
            if [ "$path_status" = failed ]; then
                say "Could not change $profile, so the shell cannot find ia-cli yet."
            else
                say "The shell cannot find ia-cli yet, because $install_dir is not on PATH."
            fi
            say "Add this line to your shell's startup file:"
            say "    $path_line"
            say ""
            say "${bold}Next:${reset} add that line, $new_window, then type: ia-cli --version"
            ;;
    esac

    # Only when the install folder was already on PATH: otherwise the line
    # just added puts it first, and an older copy elsewhere does not matter.
    if [ "$path_status" = ready ]; then
        other=$(command -v ia-cli 2>/dev/null || true)
        if [ -n "$other" ] && [ "$(real_dir "${other%/*}")" != "$(real_dir "$install_dir")" ]; then
            say ""
            say "Note: another ia-cli is at $other and comes first on PATH,"
            say "so typing ia-cli runs that one. Delete it to use this one."
        fi
    fi
}

main() {
    setup_colours
    if [ -z "${HOME:-}" ]; then
        die "The HOME variable is not set, so there is no home folder to install into."
    fi
    home=$HOME
    if [ "$home" != / ]; then
        home=${home%/}
    fi
    trap cleanup EXIT
    trap 'exit 1' HUP INT TERM

    detect_asset
    need_tools
    choose_base_url
    choose_install_dir
    tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/ia-install.XXXXXX") ||
        die "Cannot create a temporary folder."
    download_and_verify
    install_binary
    update_path
    report
}

main "$@"
