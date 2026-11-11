//! Two build-time facts for `artcraft-launcher`.
//!
//! 1. publik's app token (`src/publik.rs` includes it): `PUBLIK_APP_TOKEN` in the environment, or
//!    else `publik-app-token.txt` at the repo root, or else empty (the gate then says that this
//!    build has no publik app token). The token is public by design: it can only mint capped keys
//!    that publik attributes to the launcher.
//! 2. Windows only: embed the app icon and version info (VERSIONINFO) into `artcraft-launcher.exe`.
//!    On every other target this part does nothing. A missing resource compiler is a warning, so a
//!    cross-compile from macOS or Linux still links, unless `ARTCRAFT_LAUNCHER_REQUIRE_WINRES=1`
//!    (set by the release packaging) turns it into an error.

use std::path::Path;

const TOKEN_FILE: &str = "../../publik-app-token.txt";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/app-icon/artcraft-launcher.ico");
    println!("cargo:rerun-if-env-changed=ARTCRAFT_LAUNCHER_REQUIRE_WINRES");
    write_publik_app_token();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../assets/app-icon/artcraft-launcher.ico")
        .set("ProductName", "ArtCraft Launcher")
        .set("FileDescription", "ArtCraft Launcher")
        .set("CompanyName", "Learning Machines LLC")
        .set("LegalCopyright", "Copyright (c) the ArtCraft Launcher authors. MIT OR Apache-2.0.")
        .set("OriginalFilename", "artcraft-launcher.exe")
        .set("InternalName", "artcraft-launcher");
    if let Err(e) = res.compile() {
        if std::env::var_os("ARTCRAFT_LAUNCHER_REQUIRE_WINRES").is_some() {
            println!("cargo:warning=embedding Windows resources failed: {e}");
            std::process::exit(1);
        }
        println!("cargo:warning=artcraft-launcher.exe built without icon/version resources: {e}");
    }
}

/// Writes `$OUT_DIR/publik-app-token.txt` for `include_str!` in `src/publik.rs`.
fn write_publik_app_token() {
    println!("cargo:rerun-if-env-changed=PUBLIK_APP_TOKEN");
    println!("cargo:rerun-if-changed={TOKEN_FILE}");
    let token = std::env::var("PUBLIK_APP_TOKEN")
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .or_else(|| std::fs::read_to_string(TOKEN_FILE).ok().map(|t| t.trim().to_string()))
        .unwrap_or_default();
    if token.is_empty() {
        println!("cargo:warning=no publik app token (PUBLIK_APP_TOKEN or {TOKEN_FILE}): this build cannot sign in with publik");
    }
    let Some(out_dir) = std::env::var_os("OUT_DIR") else {
        println!("cargo:warning=OUT_DIR is not set: the publik app token cannot be written");
        std::process::exit(1);
    };
    if let Err(e) = std::fs::write(Path::new(&out_dir).join("publik-app-token.txt"), token) {
        println!("cargo:warning=the publik app token could not be written to OUT_DIR: {e}");
        std::process::exit(1);
    }
}
