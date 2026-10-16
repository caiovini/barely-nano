fn main() {
    println!("cargo:rustc-link-arg=-Tlinker-smode.ld");
    // Re-run if these change
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../linker-smode.ld");
}
