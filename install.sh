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
#      "# added by the ia-cli installer", to each startup file a new shell
#      may read, for zsh, bash and fish alike, as `pipx ensurepath` does:
#        zsh   ~/.zprofile and ~/.zshrc               (if zsh is installed)
#        bash  ~/.bashrc, and ~/.bash_profile, ~/.bash_login or ~/.profile
#              (the first that exists; ~/.profile if none does)
#        fish  ~/.config/fish/conf.d/ia-cli.fish      (if fish is installed)
#      Each line is added once and guarded, so a file read twice does not
#      put the folder on PATH twice.
# It never uses sudo and writes nothing outside $HOME unless IA_INSTALL_DIR
# says so. Running it again updates to the latest release.
#
# Uninstall: delete the binary, the lines ending in
# "# added by the ia-cli installer", and the file ia-cli.fish if there is one.
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

# download URL FILE: sets http_code (curl only; 000 when no answer). Errors
# are reported by the caller in plain words, so the tools stay quiet.
download() {
    http_code=""
    if has curl; then
        if [ -n "${IA_INSTALL_BASE_URL:-}" ]; then
            http_code=$(curl -fsL --retry 3 -w '%{http_code}' -o "$2" "$1" 2>/dev/null) || return 1
        else
            http_code=$(curl --proto '=https' --tlsv1.2 -fsL --retry 3 -w '%{http_code}' \
                -o "$2" "$1" 2>/dev/null) || return 1
        fi
    else
        wget -q -O "$2" "$1" 2>/dev/null
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
        case $http_code in
            "")
                if [ -n "${IA_VERSION:-}" ]; then
                    die "The download failed. Check the internet connection, and check that" \
                        "$IA_VERSION is a release listed at $RELEASES_URL"
                fi
                die "The download failed." \
                    "Check the internet connection, then run the same command again."
                ;;
            000)
                die "The download failed: could not reach GitHub." \
                    "Check the internet connection, then run the same command again."
                ;;
            404)
                if [ -n "${IA_VERSION:-}" ]; then
                    die "The download failed: GitHub has no such file (HTTP 404)." \
                        "Check that $IA_VERSION is a release listed at $RELEASES_URL"
                fi
                die "The download failed: GitHub has no such file (HTTP 404)." \
                    "Report this at https://github.com/internetarchivecanada/ia/issues"
                ;;
            *)
                die "The download failed: GitHub answered with HTTP $http_code." \
                    "Wait a few minutes, then run the same command again."
                ;;
        esac
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
    if [ -e "$install_dir" ] && [ ! -d "$install_dir" ]; then
        die "$install_dir is a file, not a folder, so ia-cli cannot be installed in it." \
            "Rename or move that file, then run the same command again."
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
        # Relative entries (such as .) depend on the current folder; skip them.
        case $entry in /*) ;; *) continue ;; esac
        if [ -d "$entry" ] && [ "$(real_dir "$entry")" = "$want" ]; then
            found=0
            break
        fi
    done
    set +f
    IFS=$old_ifs
    return "$found"
}

# Sets sh_line and fish_line, the guarded lines that put install_dir on
# PATH, and the tokens has_path_line looks for. $HOME stays unexpanded for
# the default folder, so the lines keep working if the home folder moves.
make_path_lines() {
    if [ "$install_dir" = "$home/bin" ]; then
        # shellcheck disable=SC2016 # expands when the shell reads the line
        sh_dir='"$HOME/bin"'
        # shellcheck disable=SC2016
        sh_token='$HOME/bin'
        # shellcheck disable=SC2016
        sh_export='export PATH="$HOME/bin:$PATH"'
        # shellcheck disable=SC2016
        fish_dir='"$HOME/bin"'
    else
        sh_dir="'$(printf '%s' "$install_dir" | sed "s/'/'\\\\''/g")'"
        sh_token=$sh_dir
        # shellcheck disable=SC2016
        sh_export='export PATH='"$sh_dir"':"$PATH"'
        fish_dir="'$(printf '%s' "$install_dir" | sed "s/[\\\\']/\\\\&/g")'"
    fi
    # The guards keep PATH from growing when more than one file is read.
    # shellcheck disable=SC2016
    sh_line='case ":$PATH:" in *:'"$sh_dir"':*) ;; *) '"$sh_export"' ;; esac '"$PROFILE_MARK"
    # `contains` and `set -gx` work on old fish as well as new.
    # shellcheck disable=SC2016
    fish_line='if not contains -- '"$fish_dir"' $PATH; set -gx PATH '"$fish_dir"' $PATH; end '"$PROFILE_MARK"
}

# has_path_line FILE TOKEN: FILE has a line this installer added for this
# folder (including the shorter line earlier versions wrote), not commented out.
has_path_line() {
    [ -f "$1" ] && grep -F "$PROFILE_MARK" "$1" 2>/dev/null |
        grep -v '^[[:space:]]*#' | grep -qF "$2"
}

# append_line FILE LINE: add LINE at the end of FILE on a line of its own,
# creating the file (and its folder) if needed.
append_line() {
    if [ -e "$1" ] && { [ ! -r "$1" ] || [ ! -w "$1" ]; }; then
        return 1
    fi
    mkdir -p "${1%/*}" 2>/dev/null || return 1
    separator=""
    if [ -s "$1" ] && [ -n "$(tail -c 1 "$1" 2>/dev/null)" ]; then
        separator="
"
    fi
    printf '%s%s\n' "$separator" "$2" 2>/dev/null >>"$1"
}

# short_path FILE: FILE with the home folder shown as ~.
short_path() {
    # shellcheck disable=SC2088 # a ~ for display, not for expansion
    case $1 in
        "$home"/*) printf '~/%s' "${1#"$home"/}" ;;
        *) printf '%s' "$1" ;;
    esac
}

# set_path_in FILE LINE TOKEN: make sure FILE has the line; record the
# result for the report. Returns 1, and adds FILE to shell_failed, if the
# file could not be changed (no permission, or a folder by that name).
set_path_in() {
    if has_path_line "$1" "$3"; then
        unchanged_files="$unchanged_files${unchanged_files:+, }$(short_path "$1")"
        path_set=1
    elif [ ! -d "$1" ] && append_line "$1" "$2"; then
        changed_files="$changed_files${changed_files:+, }$(short_path "$1")"
        path_set=1
    else
        failed_files="$failed_files${failed_files:+, }$(short_path "$1")"
        shell_failed="$shell_failed${shell_failed:+ and }$(short_path "$1")"
        return 1
    fi
}

# Puts install_dir on PATH for zsh, bash and fish, whichever the user runs,
# as `pipx ensurepath` (userpath) does: every file a new shell of each kind
# may read gets one marked line. Each shell's files are written when that
# shell is installed, already has a startup file, or is the login shell.
# Sets zsh_failed, bash_failed and fish_failed (files that could not be
# changed; "-" when that shell was skipped), and path_status: ready (already
# on PATH), changed, or manual (see choose_path_status).
update_path() {
    changed_files=""
    unchanged_files=""
    failed_files=""
    path_set=""
    zsh_failed=-
    bash_failed=-
    fish_failed=-
    login_shell=${SHELL:-}
    login_shell=${login_shell##*/}
    make_path_lines
    if on_path; then
        path_status=ready
        return 0
    fi

    # zsh: .zprofile for login shells (macOS Terminal), .zshrc for the
    # others (most Linux terminals). ZDOTDIR counts only when exported: one
    # set in ~/.zshenv without export is not seen here, and the home folder
    # is used. A relative ZDOTDIR or XDG_CONFIG_HOME is ignored, as the
    # shells do.
    zdot=${ZDOTDIR:-$home}
    case $zdot in /*) ;; *) zdot=$home ;; esac
    if has zsh || [ "$login_shell" = zsh ] || [ -e "$zdot/.zprofile" ] || [ -e "$zdot/.zshrc" ]; then
        shell_failed=""
        set_path_in "$zdot/.zprofile" "$sh_line" "$sh_token" || true
        set_path_in "$zdot/.zshrc" "$sh_line" "$sh_token" || true
        zsh_failed=$shell_failed
    fi

    # bash: the login file bash reads (macOS Terminal, ssh), which is the
    # first of these that exists, and .bashrc (most Linux terminals).
    if has bash || [ "$login_shell" = bash ] || [ -e "$home/.bashrc" ] || [ -e "$home/.bash_profile" ]; then
        login_file="$home/.profile"
        for f in .bash_login .bash_profile; do
            if [ -f "$home/$f" ]; then
                login_file="$home/$f"
            fi
        done
        shell_failed=""
        set_path_in "$login_file" "$sh_line" "$sh_token" || true
        set_path_in "$home/.bashrc" "$sh_line" "$sh_token" || true
        bash_failed=$shell_failed
    fi

    # fish: conf.d is read by every fish shell.
    config_home=${XDG_CONFIG_HOME:-$home/.config}
    case $config_home in /*) ;; *) config_home=$home/.config ;; esac
    fish_config="$config_home/fish"
    if has fish || [ "$login_shell" = fish ] || [ -d "$fish_config" ]; then
        shell_failed=""
        set_path_in "$fish_config/conf.d/ia-cli.fish" "$fish_line" "$fish_dir" || true
        fish_failed=$shell_failed
    fi

    choose_path_status
}

# The "open a new terminal window" message is right only if the files of
# the user's login shell ($SHELL; zsh and bash when it is unset or another
# shell) have the line. Otherwise path_status is manual, and manual_files
# and manual_line say what to add where, in that shell's syntax.
choose_path_status() {
    path_status=changed
    manual_files=""
    manual_line=$sh_export
    case $login_shell in
        zsh) check_shells=zsh ;;
        bash) check_shells=bash ;;
        fish) check_shells=fish ;;
        *) check_shells="zsh bash" ;;
    esac
    for sh_name in $check_shells; do
        case $sh_name in
            zsh) failed=$zsh_failed ;;
            bash) failed=$bash_failed ;;
            *) failed=$fish_failed ;;
        esac
        if [ "$failed" != - ] && [ -n "$failed" ]; then
            path_status=manual
            manual_files="$manual_files${manual_files:+ and }$failed"
            if [ "$sh_name" = fish ]; then
                # shellcheck disable=SC2016 # $PATH expands in fish
                manual_line='set -gx PATH '"$fish_dir"' $PATH'
            fi
        fi
    done
    if [ "$path_set" != 1 ]; then
        path_status=manual
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
    say ""

    case $path_status in
        ready)
            say "ia-cli is ready. Type: ia-cli --version"
            # Only here: when a line was just added it puts the install
            # folder first, so an older copy elsewhere does not matter.
            other=$(command -v ia-cli 2>/dev/null || true)
            if [ -n "$other" ] && [ "$(real_dir "${other%/*}")" != "$(real_dir "$install_dir")" ]; then
                say ""
                say "Note: another ia-cli is at $other and comes first on PATH,"
                say "so typing ia-cli runs that one. Delete it to use this one."
            fi
            ;;
        changed)
            say "${bold}Next:${reset} Open a new terminal window, then type: ia-cli --version"
            say ""
            if [ -n "$changed_files" ]; then
                say "(So new terminal windows find ia-cli, one line was added to: $changed_files)"
            fi
            if [ -n "$unchanged_files" ]; then
                say "(Already set up: $unchanged_files)"
            fi
            if [ -n "$failed_files" ]; then
                say "(Could not change: $failed_files)"
            fi
            ;;
        manual)
            say "ia-cli is installed, but new terminal windows will not find it yet."
            if [ -n "$manual_files" ]; then
                say "$manual_files could not be changed."
                say "${bold}Next:${reset} add this line to $manual_files, then open a new terminal window:"
            else
                if [ -n "$failed_files" ]; then
                    say "These files could not be changed: $failed_files"
                fi
                say "${bold}Next:${reset} add this line to your shell's startup file, then open a new terminal window:"
            fi
            say "    $manual_line"
            ;;
    esac
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
    # A TMPDIR that does not exist or cannot be written falls back to /tmp.
    tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/ia-install.XXXXXX" 2>/dev/null ||
        mktemp -d /tmp/ia-install.XXXXXX 2>/dev/null) ||
        die "Cannot create a temporary folder in ${TMPDIR:-/tmp} or /tmp."
    download_and_verify
    install_binary
    update_path
    report
}

main "$@"
