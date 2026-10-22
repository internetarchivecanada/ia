test:
    cargo test --locked -p ia-core -p ia-cli

check:
    cargo clippy --locked -p ia-core -p ia-cli -- -D warnings

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps -p ia-core -p ia-cli

audit:
    cargo audit

build-release:
    cargo build --locked -p ia-cli --release

# Run all CI checks locally (same jobs as .github/workflows/ci.yml and audit.yml)
ci: fmt-check check test doc audit

# Push main + tags to the read-only GitLab mirror (requires a 'gitlab' remote)
mirror:
    git push gitlab main --tags
