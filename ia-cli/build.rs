fn main() {
    // Emit the Rust target triple so the binary knows which release asset to download.
    // Used by the self-update feature to find the matching GitHub Release asset.
    println!(
        "cargo:rustc-env=IA_TARGET={}",
        std::env::var("TARGET").unwrap()
    );
}
