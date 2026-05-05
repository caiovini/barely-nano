fn main() {
    if cfg!(feature = "smode") {
        println!("cargo:rustc-link-arg=-Tlinker-smode.ld");
    } else {
        println!("cargo:rustc-link-arg=-Tlinker-mmode.ld");
    }
    // Re-run if these change
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../linker-smode.ld");
    println!("cargo:rerun-if-changed=../linker-mmode.ld");
    println!("cargo:rerun-if-changed=../linker.ld");
}
