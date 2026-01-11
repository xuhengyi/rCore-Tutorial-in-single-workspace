fn main() {
    use std::{env, fs, path::PathBuf};

    let ld = &PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("linker.ld");

    // 根据 nobios feature 选择链接脚本
    #[cfg(feature = "nobios")]
    fs::write(ld, linker::SCRIPT_NOBIOS).unwrap();
    #[cfg(not(feature = "nobios"))]
    fs::write(ld, linker::SCRIPT).unwrap();

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=LOG");
    println!("cargo:rerun-if-env-changed=APP_ASM");
    println!("cargo:rustc-link-arg=-T{}", ld.display());
}
