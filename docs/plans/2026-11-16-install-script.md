# Install script

## Goal

One command installs or updates `ia-cli` on macOS and Linux:

    curl -fsSL https://raw.githubusercontent.com/internetarchivecanada/ia/main/install.sh | sh

The readers include people who cannot debug a shell error, on Intel and Apple
Silicon Macs as old as macOS 10.14, without Homebrew or administrator rights.

## Prior art

The `rustup` (`rustup-init.sh`) and `uv` (cargo-dist) installers solve the same
problems. This script follows them where they agree:

- POSIX `sh`, the whole body in functions, `main` called on the last line, so a
  download cut short runs nothing.
- `curl --proto '=https'`, `wget` as a fallback, a temporary directory removed
  on exit.
- Under Rosetta (`sysctl.proc_translated` = 1) the native Apple Silicon build is
  chosen, as `rustup` does.
- The install folder is user-owned and no `sudo` is used. `uv` installs to
  `~/.local/bin`; this script keeps `~/bin`, the folder the instructions already
  sent to users name, and which `ia-cli update` can rewrite.
- `PATH` is changed by one line in the login profile, and only when the folder
  is not on `PATH` already.

Where this script differs: it verifies the release's `SHA256SUMS` (the `uv`
installer verifies only when a checksum is embedded), and it runs the new
binary with `--version` before it replaces the old one, so a binary the
computer cannot run never replaces one that works.

## Behaviour

1. Map `uname -s`/`uname -m` to an asset: Darwin arm64 (or Rosetta) →
   `ia-aarch64-apple-darwin`; Darwin x86_64 → `ia-x86_64-apple-darwin`; Linux
   x86_64 → `ia-x86_64-unknown-linux-musl`. Anything else stops with the
   platform name and the releases page.
2. Download the asset and `SHA256SUMS` from `releases/latest/download/`, or
   from `releases/download/$IA_VERSION/` when `IA_VERSION` is set.
3. Compare the asset's SHA-256 with its line in `SHA256SUMS`. A mismatch or a
   missing line stops before anything is written outside the temporary folder.
4. Copy the binary beside the destination (`$IA_INSTALL_DIR`, default
   `$HOME/bin`), mode 755, run it with `--version`, then `mv` it over
   `ia-cli`. If it cannot run, remove the copy, print the OS version and the
   asset's minimum macOS version, and exit non-zero.
5. If the folder is not on `PATH`, append one commented line to the login
   profile: zsh → `~/.zprofile`; bash → `~/.bash_profile` if it exists, else
   `~/.bash_login` if it exists (bash would read it instead of `~/.profile`),
   else `~/.profile`; any other shell → print the line. An identical line
   already present is not added again.
6. Print what was installed, where, what file was changed, and the one next
   step.

## Test hooks

Environment variables for tests only: `IA_INSTALL_BASE_URL` (download base,
`file://` allowed), `IA_INSTALL_UNAME_S` and `IA_INSTALL_UNAME_M` (replace
`uname`).

## Tests

- `scripts/test-install.sh`: offline, with a fake release in a temporary
  folder served by `file://`. Fresh install, rerun, bash and zsh profiles, a
  home folder with spaces, checksum mismatch, missing checksum line, a binary
  that cannot run, unsupported platform. Runs the installer under `sh` and,
  where present, `dash`.
- `scripts/test-install-live.sh`: downloads the latest release from GitHub,
  installs twice into a temporary `HOME` for each profile case, and checks the
  binary runs and the profile has exactly one added line.
- CI (`.github/workflows/install.yml`): `shellcheck`, the offline test on
  Ubuntu and macOS, and the live test on Ubuntu and macOS.

## Out of scope

Windows, Homebrew, signing and notarization, Rust changes.

## Follow-up: PATH for every shell, without asking

The first version wrote one profile chosen from `$SHELL` and printed an `sh`
line for anything else. On Linux that misses most new terminals (desktop
terminals start non-login shells, which read `~/.bashrc` or `~/.zshrc`), and
the printed line is wrong for fish. The readers must never be asked about
their shell or told to edit a file, so the installer now does what
`pipx ensurepath` (the `userpath` library) does: it ignores `$SHELL` and sets
PATH for zsh, bash and fish together.

- zsh (when installed or already configured): `~/.zprofile` and `~/.zshrc`,
  as `userpath` does (`rustup` uses `~/.zshenv`); an exported `ZDOTDIR` is
  honoured.
- bash: `~/.bashrc`, and the login file bash reads (`~/.bash_profile`, else
  `~/.bash_login`, else `~/.profile`).
- fish (when installed or its config folder exists):
  `$XDG_CONFIG_HOME/fish/conf.d/ia-cli.fish`, one `if not contains ...; set -gx
  PATH ...; end` line, which works on old and new fish.
- Each line is guarded so a file read twice does not add the folder twice, is
  appended once, and a file that already has a marked line for the folder,
  including the line the first version wrote, is left alone.
- The message is the same everywhere: what was installed, then "Open a new
  terminal window, then type: ia-cli --version", then one quiet line listing
  the files changed. Only if no file could be written does it give a line to
  add by hand.
- Failed downloads report the HTTP status instead of raw `curl` output; an
  unusable `TMPDIR` falls back to `/tmp`; `~/bin` as a file has its own message.
