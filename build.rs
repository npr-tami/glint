//! Build step: bakes res/glint.rc (icons, manifest, version info) into glint.exe. The manifest is what
//! makes Glint sharp on high-DPI screens (per-monitor v2) and gives it the modern (v6) common
//! controls, which the "Filter" hint in the filter box needs.
//!
//! Two toolchains are supported:
//! - MSVC (what rustup installs on Windows by default): Microsoft's rc.exe from the Windows SDK,
//!   which the Visual Studio Build Tools put on the machine. If rc.exe can't be found, the manifest
//!   is still embedded by the linker; only the icon and version info are left out (with a warning).
//! - GNU / MinGW (cross-building from Linux): windres.
//!
//! A missing resource compiler never stops the build: Glint runs without its icon too.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=res/glint.rc");
    println!("cargo:rerun-if-changed=res/glint.manifest");
    println!("cargo:rerun-if-changed=res/logo/glint.ico");
    println!("cargo:rerun-if-changed=res/zip");
    println!("cargo:rerun-if-env-changed=RC");
    println!("cargo:rerun-if-env-changed=WINDRES");

    // Only Windows builds get resources.
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = PathBuf::from(env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    let res = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR")).join("res");
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        msvc(&res, &out);
    } else {
        gnu(&res, &out);
    }
}

/// MinGW: windres turns the .rc into an object file the linker takes like any other.
fn gnu(res: &Path, out: &Path) {
    let obj = out.join("glint_res.o");
    // On a Linux host the MinGW tool is prefixed; on Windows with MinGW it's plain `windres`.
    let windres = env::var("WINDRES").unwrap_or_else(|_| if cfg!(windows) { "windres".into() } else { "x86_64-w64-mingw32-windres".into() });
    let ok = Command::new(&windres)
        .current_dir(res)
        .args(["--input", "glint.rc", "--output-format=coff", "--output"])
        .arg(&obj)
        .status()
        .is_ok_and(|s| s.success());
    if ok {
        println!("cargo:rustc-link-arg-bins={}", obj.display());
    } else {
        println!("cargo:warning=Could not run {windres}: glint.exe is built without its icon and manifest.");
    }
}

/// MSVC: rc.exe makes a .res file, which link.exe takes directly.
fn msvc(res: &Path, out: &Path) {
    let res_file = out.join("glint.res");
    for rc in rc_candidates() {
        let ran = Command::new(&rc).current_dir(res).args(["/nologo", "/fo"]).arg(&res_file).arg("glint.rc").status();
        match ran {
            Ok(s) if s.success() => {
                println!("cargo:rustc-link-arg-bins={}", res_file.display());
                return;
            }
            Ok(_) => println!("cargo:warning={} could not compile res/glint.rc", rc.display()),
            Err(_) => {} // not there; try the next one
        }
    }
    // No resource compiler: the linker can still embed the manifest, which matters most.
    println!("cargo:warning=rc.exe (Windows SDK) not found: glint.exe gets its manifest but no icon. Install the Windows SDK, or set RC to rc.exe's path.");
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}", res.join("glint.manifest").display());
}

/// Where rc.exe may be: $RC, the PATH (a Developer Command Prompt), then the newest Windows 10/11
/// SDK in its usual place.
fn rc_candidates() -> Vec<PathBuf> {
    let mut list = Vec::new();
    if let Ok(rc) = env::var("RC") {
        list.push(PathBuf::from(rc));
    }
    list.push(PathBuf::from("rc.exe"));
    let arch = if env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("aarch64") { "arm64" } else { "x64" };
    for root in ["ProgramFiles(x86)", "ProgramFiles"].iter().filter_map(|v| env::var(v).ok()) {
        let bin = Path::new(&root).join("Windows Kits").join("10").join("bin");
        // One folder per SDK version ("10.0.22621.0"); newest first.
        let mut versions: Vec<PathBuf> = std::fs::read_dir(&bin)
            .map(|dir| dir.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.join(arch).join("rc.exe").is_file()).collect())
            .unwrap_or_default();
        versions.sort_by_key(|p| {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            name.split('.').map(|part| part.parse::<u32>().unwrap_or(0)).collect::<Vec<_>>()
        });
        list.extend(versions.into_iter().rev().map(|p| p.join(arch).join("rc.exe")));
        list.push(bin.join(arch).join("rc.exe")); // older SDKs
    }
    list
}
