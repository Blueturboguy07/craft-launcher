//! publik's release feed: `GET https://publikhq.com/api/v1/craft/release_info`.
//!
//! publik's server reads the releases of every fork (`Blueturboguy07/<app>`) on a schedule and
//! serves them in one answer, so the launcher doesn't have to ask GitHub about each app (GitHub
//! allows 60 unauthenticated requests an hour). The fork on GitHub stays the fallback: for apps
//! the feed doesn't list, apps it hasn't fetched within [`MAX_AGE`], entries that aren't one of
//! publik's builds, or when the feed doesn't answer.
//!
//! The answer has this shape (every field optional; the tests' `sample()` is a full example):
//!
//! ```text
//! { "release_info": {
//!     "apps": { "<app id>": {                       // `photocraft`…, `artcraft-launcher`
//!         "repo": "Blueturboguy07/<repo>",          // must be the app's fork
//!         "app_last_fetched": "<RFC 3339>",          // trusted for MAX_AGE after this
//!         "latest_release": null | {
//!           "tag": "v0.8.0-publik.2",                // must be `v<version>-publik.<n>`
//!           "name", "published_at" (RFC 3339), "html_url", "prerelease", "notes_markdown",
//!           "assets": [ { "name", "size_bytes",
//!                         "download_url": "https://github.com/Blueturboguy07/<repo>/releases/download/…",
//!                         "sha256": "<64 hex>" | null } ] } } },
//!     "last_fetched": "<RFC 3339>" },
//!   "app_renames": [], "repo_renames": [],
//!   "important_notices": { "announcements": [], "alerts": [] } }
//! ```
//!
//! Only publik's builds count: an entry whose tag isn't `v<version>-publik.<n>` is ignored, and a
//! release's version carries publik's build number (`0.8.0+publik.2`). Fields the launcher doesn't
//! read (`version`, an asset's `platform`) may be there too.
//!
//! The feed is input like any other: every field is optional, unknown fields are ignored, a field
//! of the wrong type reads as missing, one bad entry never spoils the others, and download links
//! must still point at the app's own fork's GitHub releases.
//!
//! A developer can serve the feed and the builds from their own machine instead (see
//! [`Feed::with_mirror`]): then links under the mirror are accepted too, and entries never go stale.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde::de::{DeserializeOwned, Deserializer};

use crate::catalog::{CraftApp, GITHUB_OWNER};
use crate::release::{self, Asset, Release, truncate};
use crate::time::parse_rfc3339;

/// Where the feed lives: publik's server, never ArtCraft's.
pub const FEED_URL: &str = "https://publikhq.com/api/v1/craft/release_info";

/// An app's entry is trusted for this long after publik's server fetched it.
pub const MAX_AGE: i64 = 24 * 3600;

/// The feed, read leniently.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Feed {
    /// By lowercase app name.
    pub apps: BTreeMap<String, FeedApp>,
    /// When the least recently fetched app was fetched (Unix seconds).
    pub last_fetched: Option<i64>,
    pub app_renames: Vec<AppRename>,
    /// Repositories that moved, both under [`GITHUB_OWNER`] (publik's account): a rename anywhere
    /// else is dropped.
    pub repo_renames: Vec<RepoRename>,
    pub announcements: Vec<Notice>,
    pub alerts: Vec<Notice>,
    /// A local mirror's address (ending in `/`): downloads under it are accepted alongside GitHub's.
    /// Never read from the feed itself; the caller sets it with [`Feed::with_mirror`].
    pub mirror: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FeedApp {
    pub repo: Option<String>,
    /// Present when the repo has a published release. Meaningless unless `fetched_at` is set.
    pub release: Option<FeedRelease>,
    /// When publik's server last fetched this app (Unix seconds).
    pub fetched_at: Option<i64>,
}

/// A release as the feed describes it, before it's checked against the app's repo.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FeedRelease {
    pub tag: String,
    pub name: String,
    pub published_at: Option<i64>,
    pub html_url: String,
    pub prerelease: bool,
    pub notes: String,
    pub assets: Vec<(String, u64, String, Option<String>)>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AppRename {
    pub old_name: String,
    pub old_name_lower: String,
    pub new_name: String,
    pub new_name_lower: String,
    pub description: String,
    pub renamed_at: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepoRename {
    pub old_repo: String,
    pub new_repo: String,
    pub renamed_at: Option<i64>,
}

/// An announcement (shown inline, dismissible) or an alert (a popup the user acknowledges).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Notice {
    /// Stable, so a dismissal sticks across launches and refreshes.
    pub id: String,
    pub title: String,
    pub body: String,
    /// Only `https://` links survive parsing.
    pub link: Option<String>,
    pub created_at: Option<i64>,
}

/// What the feed says about one app, once checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FeedAnswer {
    /// Fresh: publik's latest build of the app, or `None` when there is none.
    Fresh(Option<Release>),
    /// Not listed, never fetched, older than [`MAX_AGE`], or not one of publik's builds: ask
    /// GitHub (publik's fork).
    AskGitHub,
}

const MAX_NOTICES: usize = 20;
const MAX_TEXT: usize = 4000;

impl Feed {
    /// Reads the feed. `None` only when the answer isn't JSON (or isn't an object) at all.
    pub fn parse(json: &str) -> Option<Self> {
        let raw: RawResponse = serde_json::from_str(json).ok()?;
        let mut feed = Feed {
            apps: raw.release_info.apps.into_iter().map(|(k, v)| (k.to_lowercase(), v.into_app())).collect(),
            last_fetched: raw.release_info.last_fetched.as_deref().and_then(parse_rfc3339),
            app_renames: raw.app_renames.into_iter().filter_map(RawAppRename::into_rename).collect(),
            repo_renames: raw.repo_renames.into_iter().filter_map(RawRepoRename::into_rename).collect(),
            announcements: raw.important_notices.announcements.into_iter().filter_map(|n| n.into_notice("announcement")).take(MAX_NOTICES).collect(),
            alerts: raw.important_notices.alerts.into_iter().filter_map(|n| n.into_notice("alert")).take(MAX_NOTICES).collect(),
            mirror: None,
        };
        // App renames are news too: show them with the announcements, inside the cap.
        feed.announcements.truncate(MAX_NOTICES.saturating_sub(feed.app_renames.len()));
        for r in &feed.app_renames {
            feed.announcements.push(Notice {
                id: format!("rename:{}:{}", r.old_name_lower, r.new_name_lower),
                title: format!("{} is now {}", r.old_name, r.new_name),
                body: r.description.clone(),
                link: None,
                created_at: r.renamed_at,
            });
        }
        feed.announcements.truncate(MAX_NOTICES);
        Some(feed)
    }

    /// Trusts downloads under `base` (which must end in `/`) and keeps every entry fresh: a feed
    /// served by `cargo xtask local serve` on the developer's own machine.
    #[must_use]
    pub fn with_mirror(mut self, base: &str) -> Self {
        self.mirror = base.ends_with('/').then(|| base.to_string());
        self
    }

    /// The repo to ask GitHub about for `app`, following the feed's repo renames. Always under
    /// [`GITHUB_OWNER`]: the feed can move an app within publik's account, never out of it.
    pub fn repo_for(&self, app: &CraftApp) -> String {
        let mut repo = app.repo.to_string();
        // Follow a chain of moves (a → b → c), without looping forever.
        for _ in 0..8 {
            match self.repo_renames.iter().find(|r| r.old_repo.eq_ignore_ascii_case(&repo)) {
                Some(r) if !r.new_repo.eq_ignore_ascii_case(&repo) => repo = r.new_repo.clone(),
                _ => break,
            }
        }
        repo
    }

    /// What the feed says about `app` at `now`.
    pub fn answer(&self, app: &CraftApp, now: i64) -> FeedAnswer {
        let Some(entry) = self.apps.get(app.id) else { return FeedAnswer::AskGitHub };
        let Some(fetched_at) = entry.fetched_at else { return FeedAnswer::AskGitHub };
        // A timestamp from the future (clock skew) counts as just now; one too old doesn't count.
        // A local mirror is as fresh as its last sync.
        if self.mirror.is_none() && now.saturating_sub(fetched_at) > MAX_AGE {
            return FeedAnswer::AskGitHub;
        }
        // The feed's repo must be the app's, or one the feed says it moved to.
        let repo = self.repo_for(app);
        if let Some(feed_repo) = &entry.repo
            && !feed_repo.eq_ignore_ascii_case(&repo)
        {
            return FeedAnswer::AskGitHub;
        }
        match &entry.release {
            None => FeedAnswer::Fresh(None),
            Some(r) => match r.to_release(&repo, self.mirror.as_deref()) {
                Some(release) => FeedAnswer::Fresh(Some(release)),
                None => FeedAnswer::AskGitHub,
            },
        }
    }
}

impl FeedRelease {
    /// The release with only the downloads that belong to `repo` (or sit under `mirror`), its
    /// version marked as publik's build `n`. `None` when it isn't one of publik's builds (a tag
    /// other than `v<version>-publik.<n>`, or one with odd characters).
    fn to_release(&self, repo: &str, mirror: Option<&str>) -> Option<Release> {
        let version = release::publik_tag(&self.tag).filter(|_| release::valid_tag(&self.tag))?;
        let allowed = |name: &str, url: &str| {
            (release::download_url_under(url, &release::download_prefix(repo))
                && url == format!("{}{}/{}", release::download_prefix(repo), self.tag, name))
                || mirror.is_some_and(|m| release::download_url_under_mirror(url, m))
        };
        let releases_page = format!("https://github.com/{repo}/releases/");
        let assets = self
            .assets
            .iter()
            .filter(|(name, _, url, _)| release::valid_file_name(name) && allowed(name, url))
            .map(|(name, size, url, sha)| Asset {
                name: name.clone(),
                size: *size,
                url: url.clone(),
                sha256: sha.clone().filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit())).map(|h| h.to_ascii_lowercase()),
            })
            .collect();
        Some(Release {
            version: Some(version),
            name: if self.name.is_empty() { self.tag.clone() } else { truncate(&self.name, 120) },
            tag: self.tag.clone(),
            published_at: self.published_at,
            html_url: release::safe_release_html_url(repo, &self.html_url)
                .or_else(|| {
                    let fallback = format!("{releases_page}tag/{}", self.tag);
                    release::safe_release_html_url(repo, &fallback)
                })
                .unwrap_or_else(|| format!("https://github.com/{repo}/releases")),
            notes: truncate(&self.notes, 64 * 1024),
            prerelease: self.prerelease,
            assets,
        })
    }
}

// ---- lenient wire format ----------------------------------------------------------------------

/// Reads `T`, or its default when the value is missing, `null` or the wrong shape.
fn lenient<'de, D: Deserializer<'de>, T: DeserializeOwned + Default>(d: D) -> Result<T, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(T::deserialize(value).unwrap_or_default())
}

/// A list where entries of the wrong shape are dropped rather than spoiling the rest.
fn lenient_vec<'de, D: Deserializer<'de>, T: DeserializeOwned>(d: D) -> Result<Vec<T>, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    let items = match value {
        serde_json::Value::Array(items) => items,
        _ => return Ok(Vec::new()),
    };
    Ok(items.into_iter().filter_map(|v| T::deserialize(v).ok()).collect())
}

/// A map where entries of the wrong shape are dropped.
fn lenient_map<'de, D: Deserializer<'de>, T: DeserializeOwned>(d: D) -> Result<BTreeMap<String, T>, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    let entries = match value {
        serde_json::Value::Object(entries) => entries,
        _ => return Ok(BTreeMap::new()),
    };
    Ok(entries.into_iter().filter_map(|(k, v)| T::deserialize(v).ok().map(|t| (k, t))).collect())
}

#[derive(Deserialize, Default)]
struct RawResponse {
    #[serde(default, deserialize_with = "lenient")]
    release_info: RawReleaseInfo,
    #[serde(default, deserialize_with = "lenient_vec")]
    app_renames: Vec<RawAppRename>,
    #[serde(default, deserialize_with = "lenient_vec")]
    repo_renames: Vec<RawRepoRename>,
    #[serde(default, deserialize_with = "lenient")]
    important_notices: RawNotices,
}

#[derive(Deserialize, Default)]
struct RawReleaseInfo {
    #[serde(default, deserialize_with = "lenient_map")]
    apps: BTreeMap<String, RawApp>,
    #[serde(default, deserialize_with = "lenient")]
    last_fetched: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawApp {
    #[serde(default, deserialize_with = "lenient")]
    repo: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    latest_release: Option<RawRelease>,
    #[serde(default, deserialize_with = "lenient")]
    app_last_fetched: Option<String>,
}

impl RawApp {
    fn into_app(self) -> FeedApp {
        FeedApp {
            repo: self.repo.filter(|r| r.split('/').count() == 2 && r.split('/').all(|part| !part.is_empty() && part != "." && part != "..")),
            release: self.latest_release.and_then(RawRelease::into_release),
            fetched_at: self.app_last_fetched.as_deref().and_then(parse_rfc3339),
        }
    }
}

#[derive(Deserialize, Default)]
struct RawRelease {
    #[serde(default, deserialize_with = "lenient")]
    tag: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    name: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    published_at: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    html_url: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    prerelease: bool,
    #[serde(default, deserialize_with = "lenient")]
    notes_markdown: Option<String>,
    #[serde(default, deserialize_with = "lenient_vec")]
    assets: Vec<RawAsset>,
}

impl RawRelease {
    fn into_release(self) -> Option<FeedRelease> {
        let tag = self.tag.filter(|t| !t.is_empty() && t.len() <= 64)?;
        Some(FeedRelease {
            tag,
            name: self.name.unwrap_or_default(),
            published_at: self.published_at.as_deref().and_then(parse_rfc3339),
            html_url: self.html_url.unwrap_or_default(),
            prerelease: self.prerelease,
            notes: self.notes_markdown.unwrap_or_default(),
            assets: self.assets.into_iter().filter_map(|a| Some((a.name?, a.size_bytes, a.download_url?, a.sha256))).take(300).collect(),
        })
    }
}

#[derive(Deserialize, Default)]
struct RawAsset {
    #[serde(default, deserialize_with = "lenient")]
    name: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    size_bytes: u64,
    #[serde(default, deserialize_with = "lenient")]
    download_url: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    sha256: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawAppRename {
    #[serde(default, deserialize_with = "lenient")]
    old_name: String,
    #[serde(default, deserialize_with = "lenient")]
    old_name_lower: String,
    #[serde(default, deserialize_with = "lenient")]
    new_name: String,
    #[serde(default, deserialize_with = "lenient")]
    new_name_lower: String,
    #[serde(default, deserialize_with = "lenient")]
    description: String,
    #[serde(default, deserialize_with = "lenient")]
    renamed_at: Option<String>,
}

impl RawAppRename {
    fn into_rename(self) -> Option<AppRename> {
        (!self.old_name_lower.is_empty() && !self.new_name_lower.is_empty()).then(|| AppRename {
            old_name: if self.old_name.is_empty() { self.old_name_lower.clone() } else { truncate(&self.old_name, 80) },
            new_name: if self.new_name.is_empty() { self.new_name_lower.clone() } else { truncate(&self.new_name, 80) },
            old_name_lower: self.old_name_lower.to_lowercase(),
            new_name_lower: self.new_name_lower.to_lowercase(),
            description: truncate(&self.description, MAX_TEXT),
            renamed_at: self.renamed_at.as_deref().and_then(parse_rfc3339),
        })
    }
}

#[derive(Deserialize, Default)]
struct RawRepoRename {
    #[serde(default, deserialize_with = "lenient")]
    old_name: String,
    #[serde(default, deserialize_with = "lenient")]
    new_name: String,
    #[serde(default, deserialize_with = "lenient")]
    renamed_at: Option<String>,
}

impl RawRepoRename {
    /// `None` unless both names are `owner/name` under [`GITHUB_OWNER`]. A feed that could send
    /// an app's downloads to someone else's repository would make its checksums worthless (#90).
    fn into_rename(self) -> Option<RepoRename> {
        let ok = |r: &str| {
            let mut parts = r.split('/');
            matches!((parts.next(), parts.next(), parts.next()), (Some(a), Some(b), None) if a.eq_ignore_ascii_case(GITHUB_OWNER) && !b.is_empty() && b != "." && b != ".."
          && r.bytes().all(|c| c.is_ascii_alphanumeric() || b"-_./".contains(&c)))
        };
        (ok(&self.old_name) && ok(&self.new_name)).then(|| RepoRename {
            old_repo: self.old_name,
            new_repo: self.new_name,
            renamed_at: self.renamed_at.as_deref().and_then(parse_rfc3339),
        })
    }
}

#[derive(Deserialize, Default)]
struct RawNotices {
    #[serde(default, deserialize_with = "lenient_vec")]
    announcements: Vec<RawNotice>,
    #[serde(default, deserialize_with = "lenient_vec")]
    alerts: Vec<RawNotice>,
}

#[derive(Deserialize, Default)]
struct RawNotice {
    #[serde(default, deserialize_with = "lenient")]
    title: String,
    #[serde(default, deserialize_with = "lenient")]
    body: String,
    #[serde(default, deserialize_with = "lenient")]
    maybe_web_link: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    created_at: Option<String>,
}

impl RawNotice {
    fn into_notice(self, kind: &str) -> Option<Notice> {
        if self.title.trim().is_empty() && self.body.trim().is_empty() {
            return None;
        }
        let link = self.maybe_web_link.filter(|l| release::safe_web_link(l));
        Some(Notice {
            id: format!("{kind}:{}:{}", self.created_at.as_deref().unwrap_or(""), self.title.trim()),
            title: truncate(self.title.trim(), 200),
            body: truncate(self.body.trim(), MAX_TEXT),
            link,
            created_at: self.created_at.as_deref().and_then(parse_rfc3339),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{CraftApp, LAUNCHER};
    use crate::version;

    const NOW: i64 = 1_791_500_000; // 2026-10-09T…
    const HASH: &str = "8a5edab282632443219e051e4ade2d1d5bbc671c781051bf1437897cbdfea0f1";

    /// What publik's route serves (`/api/v1/craft/release_info`), abridged.
    fn sample() -> String {
        format!(
            r#"{{
        "success": true,
        "release_info": {{
          "apps": {{
            "photocraft": {{
              "name": "PhotoCraft", "repo": "Blueturboguy07/photocraft",
              "latest_release": {{
                "tag": "v0.6.0-publik.2", "version": "0.6.0", "name": "PhotoCraft 0.6.0 (publik build 2)",
                "published_at": "2026-10-09T10:00:00Z",
                "html_url": "https://github.com/Blueturboguy07/photocraft/releases/tag/v0.6.0-publik.2",
                "prerelease": false, "notes_markdown": "Notes",
                "assets": [
                  {{ "name": "photocraft-0.6.0-macos-universal.dmg", "size_bytes": 123,
                     "download_url": "https://github.com/Blueturboguy07/photocraft/releases/download/v0.6.0-publik.2/photocraft-0.6.0-macos-universal.dmg",
                     "sha256": "{HASH}", "platform": {{ "os": "macos", "arch": "universal", "package_type": "dmg" }} }},
                  {{ "name": "evil.dmg", "size_bytes": 1, "download_url": "https://evil.example/evil.dmg", "sha256": null }},
                  {{ "name": "photocraft-0.6.0-windows-x64.msi", "size_bytes": 1, "sha256": null,
                     "download_url": "https://github.com/storytold/photocraft/releases/download/v0.6.0/photocraft-0.6.0-windows-x64.msi" }}
                ]
              }},
              "app_last_fetched": "2026-10-09T13:00:00Z"
            }},
            "artcraft-launcher": {{
              "name": "ArtCraft Launcher", "repo": "Blueturboguy07/craft-launcher",
              "latest_release": {{
                "tag": "v0.5.1-publik.2", "version": "0.5.1", "name": "ArtCraft Launcher 0.5.1 (publik build 2)",
                "published_at": "2026-10-09T11:00:00Z",
                "html_url": "https://github.com/Blueturboguy07/craft-launcher/releases/tag/v0.5.1-publik.2",
                "prerelease": false, "notes_markdown": "",
                "assets": [
                  {{ "name": "artcraft-launcher-0.5.1-macos-universal.dmg", "size_bytes": 456,
                     "download_url": "https://github.com/Blueturboguy07/craft-launcher/releases/download/v0.5.1-publik.2/artcraft-launcher-0.5.1-macos-universal.dmg",
                     "sha256": "{HASH}", "platform": {{ "os": "macos", "arch": "universal", "package_type": "dmg" }} }}
                ]
              }},
              "app_last_fetched": "2026-10-09T13:00:00Z"
            }},
            "soundcraft": {{ "name": "SoundCraft", "repo": "Blueturboguy07/soundcraft", "latest_release": null,
                             "app_last_fetched": "2026-10-09T13:00:00Z" }},
            "filmcraft": {{ "name": "FilmCraft", "repo": "Blueturboguy07/filmcraft", "latest_release": null, "app_last_fetched": null }},
            "vectorcraft": {{ "repo": "Blueturboguy07/vectorcraft", "app_last_fetched": "2026-10-01T00:00:00Z",
                              "latest_release": {{ "tag": "v0.7.0-publik.1" }} }},
            "deckcraft": {{ "repo": "Blueturboguy07/deckcraft", "app_last_fetched": "2026-10-09T13:00:00Z",
                            "latest_release": {{ "tag": "v0.7.0" }} }},
            "gridcraft": {{ "repo": "Blueturboguy07/gridcraft", "app_last_fetched": "2026-10-09T13:00:00Z",
                            "latest_release": {{ "tag": "v0.7.0-publik.x" }} }},
            "wordcraft": {{ "repo": "storytold/wordcraft", "app_last_fetched": "2026-10-09T13:00:00Z",
                            "latest_release": {{ "tag": "v0.5.0-publik.1" }} }},
            "brandnewcraft": {{ "repo": "Blueturboguy07/brandnewcraft", "app_last_fetched": "2026-10-09T13:00:00Z" }}
          }},
          "last_fetched": "2026-10-09T13:00:00Z",
          "some_future_field": {{ "whatever": [1, 2, 3] }}
        }},
        "app_renames": [],
        "repo_renames": [],
        "important_notices": {{ "announcements": [], "alerts": [] }}
      }}"#
        )
    }

    fn app(id: &str) -> &'static CraftApp {
        CraftApp::by_id(id).unwrap()
    }

    #[test]
    fn uses_fresh_publik_builds_and_falls_back_for_everything_else() {
        assert_eq!(FEED_URL, "https://publikhq.com/api/v1/craft/release_info");
        let feed = Feed::parse(&sample()).unwrap();
        // Fresh, with a publik build: only the download from the fork survives.
        let FeedAnswer::Fresh(Some(r)) = feed.answer(app("photocraft"), NOW) else { panic!() };
        assert_eq!(r.version, Some(version::publik(version::Version::new(0, 6, 0), Some(2))));
        assert!(r.is_publik());
        assert_eq!(r.version_label(), "0.6.0");
        assert_eq!(r.tag, "v0.6.0-publik.2");
        assert_eq!(r.assets.len(), 1);
        assert!(r.assets[0].url.starts_with("https://github.com/Blueturboguy07/photocraft/releases/download/v0.6.0-publik.2/"));
        assert_eq!(r.assets[0].sha256.as_deref(), Some(HASH));
        assert_eq!(r.published_at, parse_rfc3339("2026-10-09T10:00:00Z"));
        assert_eq!(r.html_url, "https://github.com/Blueturboguy07/photocraft/releases/tag/v0.6.0-publik.2");
        // The launcher's own entry.
        let FeedAnswer::Fresh(Some(r)) = feed.answer(&LAUNCHER, NOW) else { panic!() };
        assert_eq!(r.version.as_ref().map(ToString::to_string).as_deref(), Some("0.5.1+publik.2"));
        assert_eq!(
            r.assets[0].url,
            "https://github.com/Blueturboguy07/craft-launcher/releases/download/v0.5.1-publik.2/artcraft-launcher-0.5.1-macos-universal.dmg"
        );
        // Fresh, and no publik build yet.
        assert_eq!(feed.answer(app("soundcraft"), NOW), FeedAnswer::Fresh(None));
        // Never fetched, more than a day old, or not listed: ask GitHub.
        assert_eq!(feed.answer(app("filmcraft"), NOW), FeedAnswer::AskGitHub);
        assert_eq!(feed.answer(app("vectorcraft"), NOW), FeedAnswer::AskGitHub);
        assert_eq!(feed.answer(app("cadcraft"), NOW), FeedAnswer::AskGitHub);
        // Not one of publik's builds (a plain tag, a broken build number), or ArtCraft's repo
        // instead of publik's fork: ignored, so GitHub (the fork) decides.
        assert_eq!(feed.answer(app("deckcraft"), NOW), FeedAnswer::AskGitHub);
        assert_eq!(feed.answer(app("gridcraft"), NOW), FeedAnswer::AskGitHub);
        assert_eq!(feed.answer(app("wordcraft"), NOW), FeedAnswer::AskGitHub);
        // An app the launcher doesn't know is simply carried along.
        assert!(feed.apps.contains_key("brandnewcraft"));
    }

    #[test]
    fn survives_anything_the_server_might_send() {
        for junk in ["", "null", "[]", "42", "<html>502 Bad Gateway</html>", "{\"success\":tru"] {
            let feed = Feed::parse(junk);
            if let Some(feed) = feed {
                assert!(feed.apps.is_empty(), "{junk}");
            }
        }
        // Every field the wrong type: nothing panics, nothing is trusted.
        let weird = r#"{"release_info": {"apps": {"photocraft": {"repo": 7, "latest_release": "yes",
        "app_last_fetched": ["no"]}, "pdfcraft": 5}, "last_fetched": {}},
        "app_renames": "none", "repo_renames": [1, {"old_name": "a/b"}],
        "important_notices": {"announcements": [null, {"title": 3}], "alerts": "x"}}"#;
        let feed = Feed::parse(weird).unwrap();
        assert_eq!(feed.answer(app("photocraft"), NOW), FeedAnswer::AskGitHub);
        assert!(feed.repo_renames.is_empty() && feed.announcements.is_empty() && feed.alerts.is_empty());
        // An empty object is a feed that knows nothing.
        assert_eq!(Feed::parse("{}").unwrap(), Feed::default());
    }

    #[test]
    fn follows_repo_renames_but_never_trusts_a_repo_it_was_not_told_about() {
        let moved = sample()
            .replace(
                "\"repo_renames\": []",
                r#""repo_renames": [{"old_name": "blueturboguy07/photocraft", "new_name": "Blueturboguy07/photocraft-app", "renamed_at": null}]"#,
            )
            .replace(
                "\"name\": \"PhotoCraft\", \"repo\": \"Blueturboguy07/photocraft\"",
                "\"name\": \"PhotoCraft\", \"repo\": \"Blueturboguy07/photocraft-app\"",
            )
            .replace("github.com/Blueturboguy07/photocraft/releases", "github.com/Blueturboguy07/photocraft-app/releases");
        let feed = Feed::parse(&moved).unwrap();
        assert_eq!(feed.repo_for(app("photocraft")), "Blueturboguy07/photocraft-app");
        let FeedAnswer::Fresh(Some(r)) = feed.answer(app("photocraft"), NOW) else { panic!() };
        assert!(r.assets[0].url.starts_with("https://github.com/Blueturboguy07/photocraft-app/releases/download/"));
        // The same entry without the rename: the repo doesn't match, so GitHub decides.
        let unannounced =
            moved.replace(r#"{"old_name": "blueturboguy07/photocraft", "new_name": "Blueturboguy07/photocraft-app", "renamed_at": null}"#, "");
        let feed = Feed::parse(&unannounced).unwrap();
        assert_eq!(feed.answer(app("photocraft"), NOW), FeedAnswer::AskGitHub);
    }

    #[test]
    fn a_rename_out_of_publiks_account_is_dropped() {
        // Even a feed that vouches for the new repository, with matching links and checksums,
        // can't send an app's downloads to another account (#90), ArtCraft's included.
        for (owner, wire) in [("storytold", "storytold/photocraft"), ("art", "art/photocraft")] {
            let moved = sample()
                .replace(
                    "\"repo_renames\": []",
                    &format!(
                        r#""repo_renames": [{{"old_name": "Blueturboguy07/photocraft", "new_name": "{wire}", "renamed_at": null}},
                                  {{"old_name": "{wire}", "new_name": "Blueturboguy07/photocraft", "renamed_at": null}},
                                  {{"old_name": "Blueturboguy07/photocraft", "new_name": "Blueturboguy07.evil.example/photocraft", "renamed_at": null}}]"#
                    ),
                )
                .replace("\"name\": \"PhotoCraft\", \"repo\": \"Blueturboguy07/photocraft\"", &format!("\"name\": \"PhotoCraft\", \"repo\": \"{wire}\""))
                .replace("github.com/Blueturboguy07/photocraft/releases", &format!("github.com/{owner}/photocraft/releases"));
            let feed = Feed::parse(&moved).unwrap();
            assert!(feed.repo_renames.is_empty(), "{:?}", feed.repo_renames);
            assert_eq!(feed.repo_for(app("photocraft")), "Blueturboguy07/photocraft");
            assert_eq!(feed.answer(app("photocraft"), NOW), FeedAnswer::AskGitHub, "{owner}");
        }
    }

    #[test]
    fn feed_release_drops_an_asset_from_another_tag() {
        let wrong = sample().replace("releases/download/v0.6.0-publik.2/photocraft", "releases/download/v9.0.0-publik.1/photocraft");
        let FeedAnswer::Fresh(Some(r)) = Feed::parse(&wrong).unwrap().answer(app("photocraft"), NOW) else { panic!() };
        assert!(r.assets.is_empty());
    }

    #[test]
    fn a_feed_url_cannot_traverse_out_of_the_repository() {
        // The feed advertises a legitimate tag but points the asset at another repository by
        // climbing out with `../` after the approved prefix (CWE-180).
        let escaped = sample().replace(
            "https://github.com/Blueturboguy07/photocraft/releases/download/v0.6.0-publik.2/photocraft-0.6.0-macos-universal.dmg",
            "https://github.com/Blueturboguy07/photocraft/releases/download/../../../../attacker/repo/releases/download/v99/photocraft-0.6.0-macos-universal.dmg",
        );
        let FeedAnswer::Fresh(Some(r)) = Feed::parse(&escaped).unwrap().answer(app("photocraft"), NOW) else { panic!() };
        assert!(r.assets.is_empty(), "the traversing download must be rejected");
    }

    #[test]
    fn a_local_mirror_is_trusted_only_when_asked_for_and_never_goes_stale() {
        const MIRROR: &str = "http://127.0.0.1:8787/";
        let local = sample()
            .replace("https://github.com/Blueturboguy07/photocraft/releases/download/v0.6.0-publik.2/", "http://127.0.0.1:8787/photocraft/v0.6.0/");
        // Without the mirror, the local link is dropped like any other stranger's.
        let FeedAnswer::Fresh(Some(r)) = Feed::parse(&local).unwrap().answer(app("photocraft"), NOW) else { panic!() };
        assert!(r.assets.is_empty());
        let feed = Feed::parse(&local).unwrap().with_mirror(MIRROR);
        let FeedAnswer::Fresh(Some(r)) = feed.answer(app("photocraft"), NOW) else { panic!() };
        assert_eq!(r.assets.len(), 1, "the mirror's file is kept, evil.example's still isn't");
        assert_eq!(r.assets[0].url, "http://127.0.0.1:8787/photocraft/v0.6.0/photocraft-0.6.0-macos-universal.dmg");
        // A week-old sync still counts.
        assert!(matches!(feed.answer(app("photocraft"), NOW + 7 * 86_400), FeedAnswer::Fresh(Some(_))));
        // A mirror's entries are publik builds too: a plain tag isn't offered.
        assert_eq!(feed.answer(app("deckcraft"), NOW), FeedAnswer::AskGitHub);
        // Escaping the mirror's folder isn't allowed, and a base without its trailing slash isn't a mirror.
        let escaping = local.replace("http://127.0.0.1:8787/photocraft/", "http://127.0.0.1:8787/../photocraft/");
        let FeedAnswer::Fresh(Some(r)) = Feed::parse(&escaping).unwrap().with_mirror(MIRROR).answer(app("photocraft"), NOW) else { panic!() };
        assert!(r.assets.is_empty());
        assert_eq!(Feed::default().with_mirror("http://127.0.0.1:8787").mirror, None);
    }

    /// Contract check against what publik's route really serves: point `CRAFT_APPS_FEED_SAMPLE`
    /// at a response saved from `https://publikhq.com/api/v1/craft/release_info`.
    #[test]
    #[ignore = "needs a sample saved from publik's route"]
    fn reads_what_publiks_route_serves() {
        let Ok(path) = std::env::var("CRAFT_APPS_FEED_SAMPLE") else { return };
        let json = std::fs::read_to_string(path).unwrap();
        let feed = Feed::parse(&json).unwrap();
        let now = feed.last_fetched.unwrap();
        for app in [app("photocraft"), &LAUNCHER] {
            let FeedAnswer::Fresh(Some(r)) = feed.answer(app, now) else { panic!("{} wasn't fresh", app.id) };
            assert!(r.is_publik(), "{}", app.id);
            assert!(!r.assets.is_empty() && r.assets.iter().all(|a| a.sha256.is_some()), "{}", app.id);
            assert!(r.assets.iter().all(|a| a.url.starts_with(&format!("https://github.com/{}/releases/download/", app.repo))));
        }
    }

    #[test]
    fn reads_notices_and_renames() {
        let with_notices = sample()
            .replace(
                "\"announcements\": []",
                r#""announcements": [{"title": "New: CADCraft 1.0", "body": "It's here.", "created_at": "2026-10-09T09:00:00Z"}]"#,
            )
            .replace(
                "\"alerts\": []",
                r#""alerts": [
            {"title": "Security update", "body": "Update PdfCraft now.", "maybe_web_link": "https://publikhq.com/security", "created_at": null},
            {"title": "Bad link", "body": "x", "maybe_web_link": "javascript:alert(1)"},
            {"title": "Shell link", "body": "x", "maybe_web_link": "https://publikhq.com/a|calc.exe"},
            {"title": "Plain http", "body": "x", "maybe_web_link": "http://publikhq.com/security"},
            {"title": "User-info", "body": "x", "maybe_web_link": "https://publikhq.com@evil.example/"},
            {"title": "File link", "body": "x", "maybe_web_link": "file:///etc/passwd"}]"#,
            )
            .replace(
                "\"app_renames\": []",
                r#""app_renames": [{"old_name": "PdfCraft", "old_name_lower": "pdfcraft", "new_name": "DocuCraft",
            "new_name_lower": "docucraft", "description": "Same app, new name.", "renamed_at": "2026-10-09T08:00:00Z"}]"#,
            );
        let feed = Feed::parse(&with_notices).unwrap();
        assert_eq!(feed.alerts.len(), 6);
        assert_eq!(feed.alerts[0].link.as_deref(), Some("https://publikhq.com/security"));
        assert_eq!(feed.alerts[1].link, None, "only web links are kept");
        assert_eq!(feed.alerts[2].link, None, "a link with shell metacharacters is dropped");
        assert_eq!(feed.alerts[3].link, None, "plain http is dropped");
        assert_eq!(feed.alerts[4].link, None, "a link with user-info is dropped");
        assert_eq!(feed.alerts[5].link, None, "a file link is dropped");
        let titles: Vec<&str> = feed.announcements.iter().map(|n| n.title.as_str()).collect();
        assert_eq!(titles, ["New: CADCraft 1.0", "PdfCraft is now DocuCraft"]);
        // Ids are stable across parses, so dismissals stick.
        assert_eq!(Feed::parse(&with_notices).unwrap().alerts[0].id, feed.alerts[0].id);
    }

    #[test]
    fn rename_notices_count_toward_the_cap() {
        let rename = |i: usize| {
            format!(
                r#"{{"old_name": "PdfCraft{i}", "old_name_lower": "pdfcraft{i}", "new_name": "DocuCraft{i}", "new_name_lower": "docucraft{i}",
            "description": "Same app, new name.", "renamed_at": "2026-10-09T08:00:00Z"}}"#
            )
        };
        for (news, renames) in [(MAX_NOTICES, 1), (1000, 1), (0, 30)] {
            let many: Vec<String> = (0..news).map(|i| format!(r#"{{"title": "News {i}", "body": "x"}}"#)).collect();
            let moved: Vec<String> = (0..renames).map(rename).collect();
            let full = sample()
                .replace("\"announcements\": []", &format!("\"announcements\": [{}]", many.join(",")))
                .replace("\"app_renames\": []", &format!("\"app_renames\": [{}]", moved.join(",")));
            let notices = Feed::parse(&full).unwrap().announcements;
            assert_eq!(notices.len(), MAX_NOTICES, "{news} announcements, {renames} renames");
            assert!(notices.iter().any(|n| n.title == "PdfCraft0 is now DocuCraft0"), "{news} announcements: the rename was cut");
        }
    }

    #[test]
    fn a_feed_release_with_an_odd_tag_asks_github() {
        for odd in ["v0.6.0~beta-\\u202e-publik.2", "v0.6.0~beta-a/b-publik.2", "v0.6.0-publik.2\\u202e"] {
            let feed = sample().replace("\"tag\": \"v0.6.0-publik.2\"", &format!("\"tag\": \"{odd}\""));
            assert_ne!(feed, sample(), "{odd:?}");
            assert_eq!(Feed::parse(&feed).unwrap().answer(app("photocraft"), NOW), FeedAnswer::AskGitHub, "{odd:?}");
        }
    }

    /// ArtCraft Studio (`artcraft`) as publik's route serves it (trimmed from the answer of
    /// Nov 7 2026): publik's fork and publik's build, and the launcher picks that build's dmg.
    #[test]
    fn artcraft_studio_gets_publiks_build_from_the_feed() {
        let json = r#"{"success": true, "release_info": {"apps": {"artcraft": {"name": "ArtCraft", "repo": "Blueturboguy07/artcraft",
            "latest_release": {"tag": "v0.42.0-publik.5", "version": "0.42.0", "name": "ArtCraft 0.42.0 (publik build 5)",
              "published_at": "2026-11-07T09:03:14+00:00",
              "html_url": "https://github.com/Blueturboguy07/artcraft/releases/tag/v0.42.0-publik.5",
              "prerelease": false, "notes_markdown": "",
              "assets": [
                {"name": "artcraft-0.42.0-macos-universal.dmg", "size_bytes": 82845428, "sha256": null,
                 "download_url": "https://github.com/Blueturboguy07/artcraft/releases/download/v0.42.0-publik.5/artcraft-0.42.0-macos-universal.dmg",
                 "platform": {"os": "macos", "arch": "universal", "package_type": "dmg"}},
                {"name": "SHA256SUMS.txt", "size_bytes": 102, "sha256": null, "platform": null,
                 "download_url": "https://github.com/Blueturboguy07/artcraft/releases/download/v0.42.0-publik.5/SHA256SUMS.txt"}]},
            "app_last_fetched": "2026-11-07T09:15:02.439Z"}},
          "last_fetched": "2026-11-07T09:15:02.439Z"},
          "app_renames": [], "repo_renames": [], "important_notices": {}}"#;
        let feed = Feed::parse(json).unwrap();
        let studio = app("artcraft");
        let now = feed.last_fetched.unwrap();
        let FeedAnswer::Fresh(Some(r)) = feed.answer(studio, now) else { panic!("ArtCraft Studio wasn't fresh") };
        assert_eq!(r.tag, "v0.42.0-publik.5");
        assert_eq!(r.version.as_ref().map(ToString::to_string).as_deref(), Some("0.42.0+publik.5"));
        assert!(r.is_publik());
        assert_eq!(r.assets.len(), 2);
        let mac = crate::Platform { os: crate::Os::MacOs, arch: crate::Arch::Arm64 };
        let stems: Vec<&str> = studio.stems().collect();
        let (asset, kind) = crate::platform::pick_asset(&r, &stems, mac, &[crate::PackageKind::Dmg]).unwrap();
        assert_eq!((asset.name.as_str(), kind), ("artcraft-0.42.0-macos-universal.dmg", crate::PackageKind::Dmg));
        // The same entry naming ArtCraft's repo is not publik's fork: ask GitHub (publik's fork).
        let theirs = json.replace(r#""repo": "Blueturboguy07/artcraft""#, r#""repo": "storytold/artcraft""#);
        assert_ne!(theirs, json);
        assert_eq!(Feed::parse(&theirs).unwrap().answer(studio, now), FeedAnswer::AskGitHub);
    }
}
