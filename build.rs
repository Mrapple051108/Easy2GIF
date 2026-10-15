use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=app/windows.rc");
    println!("cargo:rerun-if-changed=app/assets/easy2gif.ico");
    println!("cargo:rerun-if-env-changed=WINDRES");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    // The project uses the MinGW toolchain. Link the resource only into the
    // launcher; the Qt executable compiles this same resource through CMake.
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let resource = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("windows-resource.o");
    let status = Command::new(env::var_os("WINDRES").unwrap_or_else(|| "windres".into()))
        .arg("--input")
        .arg(root.join("app/windows.rc"))
        .arg("--include-dir")
        .arg(root.join("app"))
        .args(["--output-format", "coff", "--output"])
        .arg(&resource)
        .status()
        .expect("Cannot run windres; add the MinGW bin directory to PATH");
    assert!(status.success(), "Compiling Windows icon resources failed");
    println!("cargo:rustc-link-arg-bin=forge={}", resource.display());
}
