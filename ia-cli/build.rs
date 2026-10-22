fn main() {
    // Emit the Rust target triple so the binary knows which release asset to download.
    // `ia update` uses it to find the matching GitHub Release asset.
    println!(
        "cargo:rustc-env=IA_TARGET={}",
        std::env::var("TARGET").unwrap()
    );
}
