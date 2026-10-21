//! What the UI knows about each app.

use artcraft_launcher_core::platform::{Platform, pick_asset};
use artcraft_launcher_core::protocol::{JobPhase, LatestRelease, PausedDownload};
use artcraft_launcher_core::release::Release;
use artcraft_launcher_core::status::{self, AppStatus, Latest};
use artcraft_launcher_core::{CraftApp, Installed, version};

use crate::i18n::t;

#[derive(Clone, Debug)]
pub struct AppEntry {
    pub app: &'static CraftApp,
    pub latest: Option<LatestRelease>,
    pub installed: Option<Installed>,
    /// Detection has answered at least once.
    pub detected: bool,
    pub job: Option<JobPhase>,
    /// The service `latest` came from.
    pub origin: Option<artcraft_launcher_core::protocol::Origin>,
    /// A download waiting to be resumed.
    pub paused: Option<PausedDownload>,
    /// A finished download waiting for another try at installing it.
    pub ready: bool,
}

impl AppEntry {
    pub fn new(app: &'static CraftApp) -> Self {
        Self { app, latest: None, installed: None, detected: false, job: None, origin: None, paused: None, ready: false }
    }

    pub fn release(&self) -> Option<&Release> {
        match &self.latest {
            Some(LatestRelease::Found(r)) => Some(r),
            _ => None,
        }
    }

    /// Whether the latest release has something for `platform`.
    pub fn has_download(&self, platform: Option<Platform>) -> bool {
        let (Some(r), Some(p)) = (self.release(), platform) else { return false };
        let stems: Vec<&str> = self.app.stems().collect();
        pick_asset(r, &stems, p, p.download_kinds()).is_some()
    }

    pub fn status(&self, platform: Option<Platform>, manage_apps: bool) -> AppStatus {
        if manage_apps && !self.detected {
            return AppStatus::Checking;
        }
        let latest = match &self.latest {
            None => Latest::Pending,
            Some(LatestRelease::Failed(_)) => Latest::Failed,
            Some(LatestRelease::None) => Latest::NoRelease,
            Some(LatestRelease::Found(r)) => Latest::Found { version: r.version.as_ref(), has_asset: self.has_download(platform) },
        };
        status::status(self.installed.as_ref(), latest)
    }

    pub fn installed_label(&self) -> Option<String> {
        self.installed.as_ref().map(|i| i.version.as_ref().map(version::label).unwrap_or_else(|| t("Installed").to_string()))
    }

    /// Whether the version shown for this app is publik's own build: the latest release's when
    /// nothing is installed or an update waits, else the installed copy's.
    pub fn publik_build(&self, status: &AppStatus) -> bool {
        match (&self.installed, status) {
            (None, _) | (Some(_), AppStatus::UpdateAvailable) => self.release().is_some_and(Release::is_publik),
            (Some(i), _) => i.version.as_ref().is_some_and(version::is_publik),
        }
    }

    pub fn latest_label(&self) -> Option<String> {
        self.release().map(Release::version_label)
    }

    /// [`AppEntry::latest_label`] beside `installed` (shown the same way): a later publik build of
    /// the installed version shows its build number (`0.8.0-publik.2`), so an update never reads
    /// "0.8.0 → 0.8.0".
    pub fn latest_label_beside(&self, installed: Option<&str>) -> Option<String> {
        let label = self.latest_label()?;
        let build = self.release().and_then(|r| r.version.as_ref()).and_then(version::publik_number);
        Some(match build {
            Some(n) if installed == Some(label.as_str()) => format!("{label}-publik.{n}"),
            _ => label,
        })
    }

    pub fn failure(&self) -> Option<&str> {
        match &self.latest {
            Some(LatestRelease::Failed(why)) => Some(why),
            _ => None,
        }
    }
}

/// `text` (a version) and "· publik build" when the version it shows is publik's own build.
pub fn mark_publik(text: String, e: &AppEntry, status: &AppStatus) -> String {
    if text.trim().is_empty() || !e.publik_build(status) {
        return text;
    }
    format!("{text} · {}", t("publik build"))
}

/// The colour role of a status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Good,
    Warn,
    Info,
    Neutral,
    Faint,
}

pub fn status_text(status: &AppStatus, manage_apps: bool) -> (String, Tone) {
    match status {
        AppStatus::Checking => (t("Checking…").to_string(), Tone::Faint),
        AppStatus::Available if manage_apps => (t("Available to install").to_string(), Tone::Info),
        AppStatus::Available => (t("Available").to_string(), Tone::Info),
        AppStatus::UpToDate => (t("Installed · up to date").to_string(), Tone::Good),
        AppStatus::UpdateAvailable => (t("Update available").to_string(), Tone::Warn),
        AppStatus::Installed => (t("Installed").to_string(), Tone::Neutral),
        AppStatus::Unavailable => (t("Not available here").to_string(), Tone::Faint),
        AppStatus::NoRelease => (t("Coming soon").to_string(), Tone::Faint),
    }
}

/// [`status_text`], but saying so when a download is paused or reconnecting.
pub fn entry_status_text(e: &AppEntry, status: &AppStatus, manage_apps: bool) -> (String, Tone) {
    match (e.job, e.paused) {
        (Some(JobPhase::Reconnecting { .. }), _) => (t("Reconnecting…").to_string(), Tone::Warn),
        (Some(JobPhase::Downloading { .. }), _) if status.is_installed() => (t("Updating…").to_string(), Tone::Info),
        (Some(JobPhase::Downloading { .. }), _) => (t("Downloading…").to_string(), Tone::Info),
        (None, Some(_)) if status.is_installed() => (t("Update paused").to_string(), Tone::Warn),
        (None, Some(_)) => (t("Download paused").to_string(), Tone::Warn),
        _ => status_text(status, manage_apps),
    }
}

/// Sort rank for "by status": things needing attention first.
pub fn status_rank(status: &AppStatus) -> u8 {
    match status {
        AppStatus::UpdateAvailable => 0,
        AppStatus::Installed => 1,
        AppStatus::UpToDate => 2,
        AppStatus::Available => 3,
        AppStatus::Checking => 4,
        AppStatus::Unavailable => 5,
        AppStatus::NoRelease => 6,
    }
}

#[cfg(test)]
mod tests {
    use artcraft_launcher_core::InstallSource;

    use super::*;

    fn entry(installed: &str, tag: &str) -> AppEntry {
        let app = CraftApp::by_id("gridcraft").unwrap();
        let release = Release { tag: tag.into(), version: artcraft_launcher_core::release::publik_tag(tag), ..Release::default() };
        AppEntry {
            latest: Some(LatestRelease::Found(release)),
            installed: Some(Installed { version: version::parse(installed), source: InstallSource::MacApp, location: String::new(), package: None }),
            detected: true,
            ..AppEntry::new(app)
        }
    }

    #[test]
    fn a_later_build_of_the_same_version_shows_its_number() {
        let e = entry("0.8.0+publik.1", "v0.8.0-publik.2");
        assert_eq!(e.status(None, true), AppStatus::UpdateAvailable);
        let installed = e.installed_label();
        assert_eq!(e.latest_label_beside(installed.as_deref()).as_deref(), Some("0.8.0-publik.2"));
        assert_eq!(crate::card_parts::strip_version(&e, &AppStatus::UpdateAvailable), "0.8.0 → 0.8.0-publik.2 · publik build");
        // A new version reads as before.
        let e = entry("0.7.0+publik.3", "v0.8.0-publik.2");
        assert_eq!(e.latest_label_beside(Some("0.7.0")).as_deref(), Some("0.8.0"));
    }
}
