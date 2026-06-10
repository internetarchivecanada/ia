test:
    cargo test -p ia-core -p ia-cli

check:
    cargo clippy -p ia-core -p ia-cli -- -D warnings

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p ia-core -p ia-cli

audit:
    cargo audit

build-release:
    cargo build -p ia-cli --release --features self-update

# Run all CI checks locally
ci: fmt-check check test doc

# Push main + tags to the read-only GitLab mirror (requires a 'gitlab' remote)
mirror:
    git push gitlab main --tags
