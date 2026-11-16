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
#      of the user's login shell ($SHELL) reads:
#        zsh   ~/.zprofile and ~/.zshrc
#        bash  ~/.bashrc, and ~/.bash_profile, ~/.bash_login or ~/.profile
#              (the first that exists; ~/.profile if none does)
#        sh    ~/.profile
#        fish  ~/.config/fish/conf.d/ia-cli.fish
#      For other shells (tcsh, csh, ksh, nu) it prints the line to add, in
#      that shell's syntax, and the file to add it to.
# It never uses sudo and writes nothing outside $HOME unless IA_INSTALL_DIR
# says so. Running it again updates to the latest release.
#
# Uninstall: delete the binary and the lines ending in
# "# added by the ia-cli installer" (for fish, the file ia-cli.fish).
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
        case ${http_code:-000} in
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
        if [ -n "$entry" ] && [ -d "$entry" ] && [ "$(real_dir "$entry")" = "$want" ]; then
            found=0
            break
        fi
    done
    set +f
    IFS=$old_ifs
    return "$found"
}

# Sets the quoted forms of install_dir for each shell's syntax, and the
# lines that put it on PATH. $HOME stays unexpanded for the default folder,
# so the lines keep working if the home folder moves.
make_path_lines() {
    if [ "$install_dir" = "$home/bin" ]; then
        # shellcheck disable=SC2016 # expands when the shell reads the line
        sh_dir='"$HOME/bin"'
        # shellcheck disable=SC2016
        sh_token='$HOME/bin'
        # shellcheck disable=SC2016
        fish_dir='"$HOME/bin"'
        # shellcheck disable=SC2016
        csh_dir='"$HOME/bin"'
    else
        sh_dir="'$(printf '%s' "$install_dir" | sed "s/'/'\\\\''/g")'"
        sh_token=$sh_dir
        fish_dir="'$(printf '%s' "$install_dir" | sed "s/[\\\\']/\\\\&/g")'"
        csh_dir=$sh_dir
    fi
    nu_dir="\"$(printf '%s' "$install_dir" | sed 's/[\\"]/\\&/g')\""

    if [ "$install_dir" = "$home/bin" ]; then
        # shellcheck disable=SC2016 # $PATH expands when the shell reads the line
        sh_now='export PATH="$HOME/bin:$PATH"'
        # shellcheck disable=SC2016
        csh_now='setenv PATH "$HOME/bin:$PATH"'
    else
        # shellcheck disable=SC2016
        sh_now='export PATH='"$sh_dir"':"$PATH"'
        # shellcheck disable=SC2016
        csh_now='setenv PATH '"$csh_dir"':"$PATH"'
    fi
    # One line each; the case/contains guard keeps PATH from growing when
    # more than one of the files is read.
    # shellcheck disable=SC2016
    sh_line='case ":$PATH:" in *:'"$sh_dir"':*) ;; *) '"$sh_now"' ;; esac '"$PROFILE_MARK"
    # shellcheck disable=SC2016
    fish_line='if not contains -- '"$fish_dir"' $PATH; set -gx PATH '"$fish_dir"' $PATH; end '"$PROFILE_MARK"
    # shellcheck disable=SC2016
    fish_now='set -gx PATH '"$fish_dir"' $PATH'
    csh_line="$csh_now $PROFILE_MARK"
    # shellcheck disable=SC2016
    nu_line='$env.PATH = ($env.PATH | prepend '"$nu_dir"') '"$PROFILE_MARK"
    nu_now=${nu_line% "$PROFILE_MARK"}
}

# has_path_line FILE TOKEN: FILE has a line this installer added for this
# folder (including the shorter line earlier versions wrote).
has_path_line() {
    [ -f "$1" ] && grep -F "$PROFILE_MARK" "$1" 2>/dev/null | grep -qF "$2"
}

# append_line FILE LINE: add LINE to FILE on a line of its own, creating the
# file (and, for fish, its folder) if needed.
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

# set_path_in FILE LINE TOKEN KIND: make sure FILE has the line, and record what
# happened for the report.
set_path_in() {
    if has_path_line "$1" "$3"; then
        unchanged_files="$unchanged_files$1
"
    elif append_line "$1" "$2"; then
        changed_files="$changed_files$1
"
    else
        failed_files="$failed_files$1
"
        return 0
    fi
    path_set=1
    if [ "$4" = interactive ]; then
        new_terminal_enough=1
    fi
}

# The first of these that exists is the one a bash login shell reads.
bash_login_file() {
    for f in .bash_profile .bash_login; do
        if [ -f "$home/$f" ]; then
            printf '%s\n' "$home/$f"
            return 0
        fi
    done
    printf '%s\n' "$home/.profile"
}

# Puts install_dir on PATH for the login shell named by $SHELL, the shell
# new terminal windows start. Follows rustup and uv: every file a new
# shell of that kind may read gets one marked line. Sets path_status:
# ready (already on PATH), changed (a file has the line) or manual.
update_path() {
    shell_name=${SHELL:-}
    shell_name=${shell_name##*/}
    changed_files=""
    unchanged_files=""
    failed_files=""
    path_set=""
    new_terminal_enough=""
    make_path_lines
    if on_path; then
        path_status=ready
        return 0
    fi
    zdot=${ZDOTDIR:-$home}
    case $shell_name in
        zsh)
            # .zprofile: login shells (macOS Terminal). .zshrc: interactive
            # shells (most Linux terminals). ZDOTDIR counts only when
            # exported; one set inside ~/.zshenv without export is not seen.
            set_path_in "$zdot/.zprofile" "$sh_line" "$sh_token" login
            set_path_in "$zdot/.zshrc" "$sh_line" "$sh_token" interactive
            ;;
        bash)
            # The login file: macOS Terminal and ssh. .bashrc: interactive
            # non-login shells (most Linux terminals).
            set_path_in "$(bash_login_file)" "$sh_line" "$sh_token" login
            set_path_in "$home/.bashrc" "$sh_line" "$sh_token" interactive
            ;;
        sh | dash)
            set_path_in "$home/.profile" "$sh_line" "$sh_token" login
            ;;
        fish)
            # conf.d is read by every fish shell, login or not.
            set_path_in "${XDG_CONFIG_HOME:-$home/.config}/fish/conf.d/ia-cli.fish" \
                "$fish_line" "$fish_dir" interactive
            ;;
    esac
    if [ "$path_set" = 1 ]; then
        path_status=changed
    else
        path_status=manual
    fi
}

# Print each line of a newline-separated list, indented.
print_files() {
    printf '%s' "$1" | while IFS= read -r f; do
        say "    $f"
    done
}

# Sets manual_line and manual_file: what to add, and where, for a shell this
# script does not change files for.
manual_instructions() {
    case $shell_name in
        fish)
            manual_line=$fish_line
            manual_file="${XDG_CONFIG_HOME:-$home/.config}/fish/conf.d/ia-cli.fish"
            ;;
        tcsh)
            manual_line=$csh_line
            manual_file="$home/.tcshrc (or $home/.cshrc if you use that)"
            ;;
        csh)
            manual_line=$csh_line
            manual_file="$home/.cshrc"
            ;;
        nu)
            manual_line=$nu_line
            manual_file="Nushell's config.nu (type \$nu.config-path in Nushell to see where it is)"
            ;;
        ksh)
            manual_line=$sh_line
            manual_file="$home/.profile, and $home/.kshrc if you use one"
            ;;
        zsh | bash | sh | dash)
            manual_line=$sh_line
            manual_file=$(printf '%s' "$failed_files" | head -n 1)
            ;;
        *)
            manual_line=$sh_line
            manual_file="your shell's startup file (this line is for sh, bash, zsh and ksh; other shells need their own syntax)"
            ;;
    esac
}

# The line that puts install_dir on PATH in the current window, in the
# syntax of the user's shell.
now_line() {
    case $shell_name in
        fish) say "    $fish_now" ;;
        tcsh | csh) say "    $csh_now" ;;
        nu) say "    $nu_now" ;;
        *) say "    $sh_now" ;;
    esac
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
    elif [ "$path_status" = manual ] || [ "$new_terminal_enough" = 1 ]; then
        new_window="open a new terminal window"
    else
        new_window="log out and log back in"
    fi

    if [ "$path_status" = ready ]; then
        say ""
        say "ia-cli is ready. Type: ia-cli --version"
        # Only here: when a line was just added it puts the install folder
        # first, so an older copy elsewhere does not matter.
        other=$(command -v ia-cli 2>/dev/null || true)
        if [ -n "$other" ] && [ "$(real_dir "${other%/*}")" != "$(real_dir "$install_dir")" ]; then
            say ""
            say "Note: another ia-cli is at $other and comes first on PATH,"
            say "so typing ia-cli runs that one. Delete it to use this one."
        fi
        return 0
    fi

    if [ -n "$changed_files" ]; then
        say "So that the shell can find ia-cli, added one line ending in \"$PROFILE_MARK\" to:"
        print_files "$changed_files"
    fi
    if [ -n "$unchanged_files" ]; then
        say "Already set up, not changed:"
        print_files "$unchanged_files"
    fi
    if [ -n "$failed_files" ] && [ "$path_status" = changed ]; then
        say "Could not change (no permission), left as they were:"
        print_files "$failed_files"
    fi

    if [ "$path_status" = manual ]; then
        manual_instructions
        if [ -n "$failed_files" ]; then
            say "Could not change $manual_file (no permission), so the shell cannot find ia-cli yet."
        else
            say "The shell cannot find ia-cli yet, because $install_dir is not on PATH."
        fi
        say "Add this line to $manual_file:"
        say "    $manual_line"
        say ""
        say "${bold}Next:${reset} add that line, $new_window, then type: ia-cli --version"
    else
        say ""
        say "${bold}Next:${reset} $new_window, then type: ia-cli --version"
    fi
    say "Or, to use ia-cli in this window right away, paste this line first:"
    now_line
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
