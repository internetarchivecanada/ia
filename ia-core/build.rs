/// Captures the rustc version at build time so the User-Agent string can
/// report `Rust/{rust_version}` (matching the Python library's UA format).
fn main() {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let version = std::process::Command::new(&rustc)
        .arg("--version")
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        // "rustc 1.85.0 (4d91de4e4 2025-02-17)" -> "1.85.0"
        .and_then(|s| s.split_whitespace().nth(1).map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=IA_RUSTC_VERSION={version}");
    println!("cargo:rerun-if-env-changed=RUSTC");
}
