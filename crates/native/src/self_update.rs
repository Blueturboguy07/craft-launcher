//! ArtCraft Launcher updating itself.
//!
//! Delegated to [`craft_update::self_update`], except for publik's macOS build
//! (`com.publik.craftlauncher`, signed by publik's team): `craft_update` knows only ArtCraft's
//! bundle id and team, so it would neither find that `.app` nor accept publik's signature. That
//! copy updates itself here, the same way: the new bundle from the DMG of a publik build is
//! checked (publik's team, notarization, version) and copied over this one, then reopened.

use std::ffi::OsString;

use artcraft_launcher_core::PackageKind;
use artcraft_launcher_core::platform::Platform;
use artcraft_launcher_core::protocol::JobPhase;
use artcraft_launcher_core::release::Release;
use artcraft_launcher_core::version::{self, Version};

use crate::http::{Http, Stop};
use crate::paths::Paths;

pub use craft_update::self_update::{FINISH_FLAG, Install, PRETEND_VAR, is_self_updating};

/// publik's build number of this copy: `n` of the release tag `v<version>-publik.<n>` it is
/// built for, set at build time (`ARTCRAFT_LAUNCHER_PUBLIK_BUILD=2`). Without it this copy counts
/// as the plain version, so a later publik build of the same version isn't offered (rather than
/// offered again after every update).
const PUBLIK_BUILD: Option<&str> = option_env!("ARTCRAFT_LAUNCHER_PUBLIK_BUILD");

/// The version this launcher counts as (see [`PRETEND_VAR`] and [`PUBLIK_BUILD`]).
pub fn current_version() -> Option<Version> {
    std::env::var(PRETEND_VAR).ok().and_then(|v| version::parse(&v)).or_else(|| built_version(env!("CARGO_PKG_VERSION"), PUBLIK_BUILD))
}

/// `version`, marked as publik's build `publik` when that is a build number.
fn built_version(version: &str, publik: Option<&str>) -> Option<Version> {
    let v = version::parse(version)?;
    Some(match publik.and_then(|n| n.trim().parse::<u64>().ok()) {
        Some(n) => version::publik(v, Some(n)),
        None => v,
    })
}

/// How this copy is installed, if it's one the launcher knows how to replace. `craft_update`
/// decides (a development build is none), except that publik's `.app` on macOS is a bundle too.
pub fn current() -> Option<Install> {
    let install = craft_update::self_update::current(&craft_update::suite::LAUNCHER)?;
    #[cfg(target_os = "macos")]
    if let Install::Standalone(exe) = &install
        && let Some(bundle) = publik_bundle(exe)
    {
        return Some(Install::MacApp(bundle));
    }
    Some(install)
}

/// The `.app` around `exe` when it is publik's build of the launcher.
#[cfg(target_os = "macos")]
fn publik_bundle(exe: &std::path::Path) -> Option<std::path::PathBuf> {
    let exe = exe.canonicalize().unwrap_or_else(|_| exe.to_path_buf());
    let bundle = exe.ancestors().find(|p| p.extension().is_some_and(|e| e == "app"))?;
    is_publik_app(bundle).then(|| bundle.to_path_buf())
}

/// Whether `bundle` is publik's build of the launcher (`com.publik.craftlauncher`).
#[cfg(target_os = "macos")]
fn is_publik_app(bundle: &std::path::Path) -> bool {
    let Ok(plist) = plist::Value::from_file(bundle.join("Contents/Info.plist")) else { return false };
    let id = plist.as_dictionary().and_then(|d| d.get("CFBundleIdentifier")).and_then(plist::Value::as_string);
    id.is_some_and(|id| id == artcraft_launcher_core::catalog::LAUNCHER.publik_bundle_id())
}

fn to_update_paths(paths: &Paths) -> craft_update::Paths {
    craft_update::Paths { data: paths.data.clone(), cache: paths.cache.clone() }
}

fn to_update_release(release: &Release) -> craft_update::Release {
    craft_update::Release {
        tag: release.tag.clone(),
        version: release.version.clone(),
        name: release.name.clone(),
        published_at: release.published_at,
        html_url: release.html_url.clone(),
        notes: release.notes.clone(),
        prerelease: release.prerelease,
        assets: release
            .assets
            .iter()
            .map(|a| craft_update::Asset { name: a.name.clone(), size: a.size, url: a.url.clone(), sha256: a.sha256.clone() })
            .collect(),
    }
}

fn to_update_platform(platform: Platform) -> craft_update::Platform {
    craft_update::Platform {
        os: match platform.os {
            artcraft_launcher_core::Os::Windows => craft_update::Os::Windows,
            artcraft_launcher_core::Os::MacOs => craft_update::Os::MacOs,
            artcraft_launcher_core::Os::Linux => craft_update::Os::Linux,
            artcraft_launcher_core::Os::FreeBsd => craft_update::Os::FreeBsd,
        },
        arch: match platform.arch {
            artcraft_launcher_core::Arch::X64 => craft_update::Arch::X64,
            artcraft_launcher_core::Arch::Arm64 => craft_update::Arch::Arm64,
            artcraft_launcher_core::Arch::X86 => craft_update::Arch::X86,
            artcraft_launcher_core::Arch::Riscv64 => craft_update::Arch::Riscv64,
        },
    }
}

/// Whether `release` is the version this launcher just updated itself to.
pub fn just_updated_to(paths: &Paths, release: &Release) -> bool {
    let update_paths = to_update_paths(paths);
    let update_release = to_update_release(release);
    craft_update::self_update::just_updated_to(&update_paths, &update_release)
}

/// The error of the last update, if it failed, once.
pub fn take_failure(paths: &Paths) -> Option<String> {
    let update_paths = to_update_paths(paths);
    craft_update::self_update::take_failure(&update_paths)
}

/// Downloads `release` for `platform`, puts it in place and starts the helper that relaunches
/// it once this process has exited. On success the caller must quit promptly. `last` is called
/// once the download is ready and returns when the launcher may replace itself and restart.
#[allow(clippy::too_many_arguments)]
pub fn update(
    install: &Install,
    release: &Release,
    platform: Platform,
    _http: &Http,
    paths: &Paths,
    stop: &Stop,
    phase: &mut dyn FnMut(JobPhase),
    last: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    if let Install::MacApp(bundle) = install
        && (release.is_publik() || is_publik_app(bundle))
    {
        return update_publik_app(bundle, release, platform, _http, paths, stop, phase, last);
    }
    let update_release = to_update_release(release);
    let update_platform = to_update_platform(platform);
    let update_paths = to_update_paths(paths);
    let update_http = craft_update::Http::new("ArtCraft-Launcher/SelfUpdate")?;
    let mut update_phase = |p: craft_update::JobPhase| {
        phase(match p {
            craft_update::JobPhase::Downloading { done, total } => JobPhase::Downloading { done, total },
            craft_update::JobPhase::Reconnecting { done, total } => JobPhase::Reconnecting { done, total },
            craft_update::JobPhase::Waiting => JobPhase::Waiting,
            craft_update::JobPhase::Verifying => JobPhase::Verifying,
            craft_update::JobPhase::Installing => JobPhase::Installing,
            craft_update::JobPhase::Removing => JobPhase::Removing,
        });
    };
    craft_update::self_update::update(
        &craft_update::suite::LAUNCHER,
        install,
        &update_release,
        update_platform,
        &update_http,
        &update_paths,
        stop,
        &mut update_phase,
        last,
        Some(env!("CARGO_PKG_VERSION")),
    )
}

/// [`update`] for publik's macOS build: only to a newer publik build, signed by publik's team.
#[cfg(target_os = "macos")]
#[allow(clippy::too_many_arguments)]
fn update_publik_app(
    bundle: &std::path::Path,
    release: &Release,
    platform: Platform,
    http: &Http,
    paths: &Paths,
    stop: &Stop,
    phase: &mut dyn FnMut(JobPhase),
    last: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    use artcraft_launcher_core::catalog::LAUNCHER;
    use artcraft_launcher_core::platform::pick_asset;
    use artcraft_launcher_core::{InstallSource, Installed};

    use crate::install::{self, Job};

    if !release.is_publik() {
        return Err("publik's build of ArtCraft Launcher updates only to publik's builds; download the new version instead".into());
    }
    if let (Some(rel_v), Some(cur_v)) = (release.version.as_ref(), current_version().as_ref())
        && !version::is_newer(rel_v, cur_v)
    {
        return Err(format!("ArtCraft Launcher {} is not newer than currently running version {}", release.version_label(), cur_v));
    }
    let (asset, _) = pick_asset(release, &[LAUNCHER.id], platform, &[PackageKind::Dmg])
        .ok_or_else(|| format!("ArtCraft Launcher {} has no disk image for {}", release.version_label(), platform.label()))?;
    let asset = asset.clone();
    let mut job = Job { http, paths, stop, phase, install_locations: &Default::default(), replace: false, approved_path: None, downgrade: None };
    let (file, _) = install::download(release, &asset, &mut job)?;
    // The restart ends this process: nothing else may be under way.
    last()?;
    phase(JobPhase::Installing);
    // The same record `craft_update` keeps, so [`just_updated_to`] reads it.
    let marker = paths.data.join("self-update.txt");
    if let Err(e) = crate::paths::write_atomic(&marker, format!("{}\n{}\n", release.tag, crate::now()).as_bytes()) {
        log::warn!("couldn't record the self-update: {e}");
    }
    let existing = Installed {
        version: None,
        source: InstallSource::MacApp,
        location: bundle.display().to_string(),
        package: None,
        externally_managed: false,
        origin: None,
        installation: None,
    };
    let placed =
        install::macos::install_dmg(&LAUNCHER, &file, Some(&existing), None, None, paths, &install::macos::Expect::release(&LAUNCHER, release));
    let _ = std::fs::remove_file(&file);
    let scheduled = placed.and_then(|placed| {
        crate::process::spawn_detached(relaunch_command(std::process::id(), &placed)).map_err(|e| format!("couldn't schedule the restart ({e})"))
    });
    if scheduled.is_err() {
        // The update stays on offer.
        let _ = std::fs::remove_file(&marker);
    }
    scheduled
}

/// `sh`, which waits for `pid` to exit (a minute at most) and then reopens `app`. The new
/// launcher reports its real version: a pretend one was for this run only.
#[cfg(target_os = "macos")]
fn relaunch_command(pid: u32, app: &std::path::Path) -> std::process::Command {
    let mut cmd = crate::process::command("/bin/sh");
    cmd.arg("-c")
        .arg(r#"pid=$1; shift; i=0; while kill -0 "$pid" 2>/dev/null && [ $i -lt 600 ]; do sleep 0.1; i=$((i+1)); done; exec "$@""#)
        .arg("sh")
        .arg(pid.to_string())
        .arg("/usr/bin/open")
        .arg(app)
        .env_remove(PRETEND_VAR);
    cmd
}

/// The package format updating `install` downloads: the one it was installed from. Mirrors the
/// private `Install::kind` in `craft_update::self_update`, which picks the update's asset by it.
pub fn kind(install: &Install) -> PackageKind {
    match install {
        Install::MacApp(_) => PackageKind::Dmg,
        Install::Msi(_) => PackageKind::Msi,
        Install::Portable(_) => PackageKind::PortableZip,
        Install::AppImage(_) => PackageKind::AppImage,
        Install::Package(kind, _) => match kind {
            craft_update::PackageKind::Msi => PackageKind::Msi,
            craft_update::PackageKind::PortableZip => PackageKind::PortableZip,
            craft_update::PackageKind::Dmg => PackageKind::Dmg,
            craft_update::PackageKind::AppImage => PackageKind::AppImage,
            craft_update::PackageKind::Deb => PackageKind::Deb,
            craft_update::PackageKind::Rpm => PackageKind::Rpm,
            craft_update::PackageKind::ArchPkg => PackageKind::ArchPkg,
            craft_update::PackageKind::Flatpak => PackageKind::Flatpak,
            craft_update::PackageKind::TarGz => PackageKind::TarGz,
        },
        Install::Flatpak => PackageKind::Flatpak,
        Install::Standalone(_) if cfg!(windows) => PackageKind::PortableZip,
        Install::Standalone(_) => PackageKind::TarGz,
    }
}

/// Finishes an update when this program was started as the update helper.
pub fn finish(args: &[OsString]) -> Option<Result<(), String>> {
    craft_update::self_update::finish(&craft_update::suite::LAUNCHER, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_development_build_does_not_update_itself() {
        assert_eq!(current(), None);
    }

    #[test]
    fn a_launcher_built_as_a_publik_build_is_offered_only_later_builds() {
        let build2 = built_version("0.5.1", Some("2")).unwrap();
        assert_eq!(build2.to_string(), "0.5.1+publik.2");
        let release = |tag: &str| artcraft_launcher_core::release::publik_tag(tag).unwrap();
        assert!(version::is_newer(&release("v0.5.1-publik.3"), &build2));
        assert!(!version::is_newer(&release("v0.5.1-publik.2"), &build2));
        assert!(!version::is_newer(&release("v0.5.1-publik.1"), &build2));
        // Built without a number: only a higher version is newer, so nothing is offered forever.
        let plain = built_version("0.5.1", None).unwrap();
        assert_eq!(plain.to_string(), "0.5.1");
        assert!(!version::is_newer(&release("v0.5.1-publik.3"), &plain));
        assert!(version::is_newer(&release("v0.5.2-publik.1"), &plain));
        assert_eq!(built_version("0.5.1", Some("x")), Some(plain));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn publiks_app_is_found_by_its_bundle_id_and_updates_only_to_publiks_builds() {
        let dir = tempfile::tempdir().unwrap();
        let app = |name: &str, id: &str| {
            let bundle = dir.path().join(name);
            std::fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
            let mut plist = plist::Dictionary::new();
            plist.insert("CFBundleIdentifier".into(), id.into());
            plist::Value::Dictionary(plist).to_file_xml(bundle.join("Contents/Info.plist")).unwrap();
            let exe = bundle.join("Contents/MacOS/artcraft-launcher");
            std::fs::write(&exe, b"").unwrap();
            (bundle.canonicalize().unwrap(), exe)
        };
        let (publik, exe) = app("publik/ArtCraft Launcher.app", "com.publik.craftlauncher");
        assert_eq!(publik_bundle(&exe), Some(publik.clone()));
        let (_, exe) = app("artcraft/ArtCraft Launcher.app", "ai.storyteller.artcraft-launcher");
        assert_eq!(publik_bundle(&exe), None, "ArtCraft's own build is craft_update's");
        // Never ArtCraft's build over publik's.
        let artcraft = Release { tag: "v99.0.0".into(), version: version::parse("99.0.0"), ..Release::default() };
        let platform = Platform { os: artcraft_launcher_core::Os::MacOs, arch: artcraft_launcher_core::Arch::Arm64 };
        let paths = Paths::under(dir.path());
        let err = update(
            &Install::MacApp(publik),
            &artcraft,
            platform,
            &Http::new().unwrap(),
            &paths,
            &Stop::default(),
            &mut |_| {},
            &|| -> Result<(), String> { panic!("nothing may be replaced") },
        )
        .unwrap_err();
        assert!(err.contains("only to publik's builds"), "{err}");
    }

    #[test]
    fn an_update_downloads_the_format_the_copy_was_installed_from() {
        use std::path::PathBuf;
        assert_eq!(kind(&Install::Msi(PathBuf::new())), PackageKind::Msi);
        assert_eq!(kind(&Install::Portable(PathBuf::new())), PackageKind::PortableZip);
        assert_eq!(kind(&Install::MacApp(PathBuf::new())), PackageKind::Dmg);
        assert_eq!(kind(&Install::AppImage(PathBuf::new())), PackageKind::AppImage);
        assert_eq!(kind(&Install::Package(craft_update::PackageKind::Rpm, PathBuf::new())), PackageKind::Rpm);
        assert_eq!(kind(&Install::Flatpak), PackageKind::Flatpak);
        let standalone = if cfg!(windows) { PackageKind::PortableZip } else { PackageKind::TarGz };
        assert_eq!(kind(&Install::Standalone(PathBuf::new())), standalone);
    }
}
