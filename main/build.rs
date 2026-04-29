fn main() {
    if cfg!(feature = "smode") {
        println!("cargo:rustc-link-arg=-Tlinker-smode.ld");
    } else {
        println!("cargo:rustc-link-arg=-Tlinker-mmode.ld");
    }
    // Re-run if this script changes
    println!("cargo:rerun-if-changed=build.rs");
}
