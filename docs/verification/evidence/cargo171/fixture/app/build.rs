fn main() {
    let out = std::env::var("OUT_DIR").unwrap();
    std::fs::write(format!("{out}/gen.rs"), "pub const G: u32 = 7;").unwrap();
    println!("cargo:rerun-if-changed=build.rs");
}
