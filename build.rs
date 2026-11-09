//! Sous Windows avec la chaîne gnullvm, Rust ne fournit pas de bibliothèque d'import pour OpenGL.
//! On la génère ici avec rust-lld, pour compiler sans Visual Studio ni MinGW.

use std::{env, fs, path::PathBuf, process::Command};

const OPENGL32_DEF: &str = "LIBRARY opengl32.dll
EXPORTS
wglCopyContext
wglCreateContext
wglCreateLayerContext
wglDeleteContext
wglDescribeLayerPlane
wglGetCurrentContext
wglGetCurrentDC
wglGetLayerPaletteEntries
wglGetProcAddress
wglMakeCurrent
wglRealizeLayerPalette
wglSetLayerPaletteEntries
wglShareLists
wglSwapLayerBuffers
wglUseFontBitmapsA
wglUseFontBitmapsW
wglUseFontOutlinesA
wglUseFontOutlinesW
";

/// L'installateur embarque l'archive indiquée par EUHPEN_PAYLOAD. Sans elle (tests, CI), une
/// archive vide le fait compiler, et il refuse alors d'installer.
fn payload() {
    println!("cargo:rerun-if-env-changed=EUHPEN_PAYLOAD");
    if env::var_os("CARGO_FEATURE_INSTALLER").is_none() {
        return;
    }
    match env::var("EUHPEN_PAYLOAD") {
        Ok(path) if !path.is_empty() => {
            println!("cargo:rerun-if-changed={path}");
            println!("cargo:rustc-env=EUHPEN_PAYLOAD={path}");
        }
        _ => {
            let empty = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("sans-application.zip");
            fs::write(&empty, b"").expect("écriture de l'archive vide");
            println!("cargo:rustc-env=EUHPEN_PAYLOAD={}", empty.display());
        }
    }
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    payload();
    let target = env::var("TARGET").unwrap_or_default();
    if target != "x86_64-pc-windows-gnullvm" {
        return;
    }

    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let def = out.join("opengl32.def");
    fs::write(&def, OPENGL32_DEF).expect("écriture de opengl32.def");

    let rustc = env::var_os("RUSTC").expect("RUSTC");
    let sysroot = Command::new(rustc).args(["--print", "sysroot"]).output().expect("rustc --print sysroot");
    let sysroot = String::from_utf8(sysroot.stdout).expect("chemin du sysroot");
    let lld = PathBuf::from(sysroot.trim()).join("lib/rustlib").join(&target).join("bin/rust-lld.exe");

    let status = Command::new(&lld)
        .args(["-flavor", "link", "/machine:x64"])
        .arg(format!("/def:{}", def.display()))
        .arg(format!("/implib:{}", out.join("libopengl32.a").display()))
        .status()
        .unwrap_or_else(|e| panic!("{} : {e}", lld.display()));
    assert!(status.success(), "rust-lld n'a pas pu créer libopengl32.a");
    println!("cargo:rustc-link-search=native={}", out.display());
}
