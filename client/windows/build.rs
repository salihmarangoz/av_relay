// SPDX-License-Identifier: GPL-3.0-or-later
use std::{env, fs, path::PathBuf, process::Command};
fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let assets = root
        .join("../../assets/generated/windows")
        .canonicalize()
        .unwrap();
    println!("cargo:rerun-if-changed={}", assets.display());
    println!("cargo:rerun-if-changed=app.manifest");
    let quote = |path: PathBuf| {
        path.to_string_lossy()
            .trim_start_matches(r"\\?\")
            .replace('\\', "/")
    };
    let mut rc = format!(
        "1 ICON \"{}\"\n1 24 \"{}\"\n",
        quote(assets.join("app.ico")),
        quote(root.join("app.manifest"))
    );
    for (index, state) in ["off", "on", "live", "disabled"].iter().enumerate() {
        for (theme_index, theme) in ["light", "dark"].iter().enumerate() {
            rc.push_str(&format!(
                "{} ICON \"{}\"\n",
                101 + index * 2 + theme_index,
                quote(assets.join(format!("tray-{state}-{theme}.ico")))
            ));
        }
    }
    fs::write(out.join("app.rc"), rc).unwrap();
    let result = Command::new("rc.exe")
        .args(["/nologo", "/fo"])
        .arg(out.join("app.res"))
        .arg(out.join("app.rc"))
        .status()
        .expect("rc.exe not found: use scripts/build.ps1 or a Visual Studio developer shell");
    assert!(result.success(), "Windows resource compilation failed");
    println!(
        "cargo:rustc-link-arg-bin=av-relay-windows={}",
        out.join("app.res").display()
    );
}
