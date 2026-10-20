//! Where release information comes from.
//!
//! [`GitHubSource`] asks the GitHub REST API (one request per app, revalidated with ETags) and,
//! when the API is out of quota, falls back to github.com's release pages: the
//! `releases/latest` redirect names the tag and `SHA256SUMS.txt` lists every file with its
//! checksum, which costs no API quota at all. A later [`ReleaseSource`] will ask the ArtCraft
//! catalog API for every app in one request.
//!
//! The same routes find publik's own build of an app in publik's fork ([`ReleaseSource::publik`]):
//! the newest release tagged `…-publik.<n>` (`release::parse_publik_release_list`).

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use artcraft_launcher_core::protocol::{LatestRelease, Source};
use artcraft_launcher_core::release::{self, Release};
use serde::{Deserialize, Serialize};

use crate::http::{Fetched, Http};

/// One answer from a source.
#[derive(Clone, Debug)]
pub struct Answer {
    pub latest: LatestRelease,
    pub source: Source,
}

/// Looks up latest releases. Called from worker threads.
pub trait ReleaseSource: Send + Sync {
    fn latest(&self, repo: &str, prereleases: bool) -> Answer;
    /// publik's newest build in its fork `repo` (`Blueturboguy07/<app>`): the newest release
    /// tagged `…-publik.<n>`, or none. Saved like [`ReleaseSource::latest`], so
    /// [`ReleaseSource::cached`] answers for the fork too. A source that can't tell finds none.
    fn publik(&self, _repo: &str, _prereleases: bool) -> Answer {
        Answer { latest: LatestRelease::None, source: Source::Cache }
    }
    /// The last answer saved for `repo`, for an instant first paint.
    fn cached(&self, repo: &str, prereleases: bool) -> Option<LatestRelease>;
    /// API refused until this time (Unix seconds).
    fn rate_limited_until(&self) -> Option<i64>;
    /// Persist anything worth keeping.
    fn save(&self);
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Entry {
    etag: Option<String>,
    /// `None`: the repo has no releases.
    release: Option<Release>,
    prereleases: bool,
    fetched_at: i64,
}

#[derive(Default, Serialize, Deserialize)]
struct CacheFile {
    entries: BTreeMap<String, Entry>,
}

pub struct GitHubSource {
    http: Http,
    cache: Mutex<CacheFile>,
    cache_path: std::path::PathBuf,
    limited_until: Mutex<Option<i64>>,
}

impl GitHubSource {
    pub fn new(http: Http, cache_path: std::path::PathBuf) -> Self {
        let cache = std::fs::read(&cache_path).ok().and_then(|b| serde_json::from_slice::<CacheFile>(&b).ok()).unwrap_or_default();
        Self { http, cache: Mutex::new(cache), cache_path, limited_until: Mutex::new(None) }
    }

    fn entry(&self, repo: &str, prereleases: bool) -> Option<Entry> {
        let cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        cache.entries.get(repo).filter(|e| e.prereleases == prereleases).cloned()
    }

    fn store(&self, repo: &str, entry: Entry) {
        self.cache.lock().unwrap_or_else(PoisonError::into_inner).entries.insert(repo.to_string(), entry);
    }

    fn limited(&self) -> bool {
        let until = *self.limited_until.lock().unwrap_or_else(PoisonError::into_inner);
        until.is_some_and(|u| u > crate::now())
    }

    /// `publik`: the newest publik build in the list, not GitHub's latest release.
    fn ask_api(&self, repo: &str, prereleases: bool, publik: bool) -> Result<Option<Answer>, String> {
        let url = if prereleases || publik {
            format!("https://api.github.com/repos/{repo}/releases?per_page=30")
        } else {
            format!("https://api.github.com/repos/{repo}/releases/latest")
        };
        let saved = self.entry(repo, prereleases);
        let found =
            |release: Option<Release>| Answer { latest: release.map_or(LatestRelease::None, LatestRelease::Found), source: Source::GitHubApi };
        match self.http.get_api(&url, saved.as_ref().and_then(|e| e.etag.as_deref()))? {
            Fetched::Body { text, etag } => {
                let release = if publik {
                    release::parse_publik_release_list(&text, repo, prereleases)?
                } else if prereleases {
                    release::parse_github_release_list(&text, repo, true)?
                } else {
                    Some(release::parse_github_release(&text, repo)?)
                };
                self.store(repo, Entry { etag, release: release.clone(), prereleases, fetched_at: crate::now() });
                Ok(Some(found(release)))
            }
            Fetched::NotModified => match saved {
                Some(e) => {
                    self.store(repo, Entry { fetched_at: crate::now(), ..e.clone() });
                    Ok(Some(found(e.release)))
                }
                None => Err("GitHub said nothing changed, but there's no saved answer".into()),
            },
            Fetched::NotFound => {
                self.store(repo, Entry { etag: None, release: None, prereleases, fetched_at: crate::now() });
                Ok(Some(found(None)))
            }
            Fetched::RateLimited { reset } => {
                *self.limited_until.lock().unwrap_or_else(PoisonError::into_inner) = Some(reset.unwrap_or_else(|| crate::now() + 15 * 60));
                Ok(None)
            }
        }
    }

    /// The quota-free route through github.com (stable releases only). With `publik`, the fork's
    /// latest release counts only when it is a publik build.
    fn ask_pages(&self, repo: &str, publik: bool) -> Result<Answer, String> {
        let location = self.http.redirect_target(&format!("https://github.com/{repo}/releases/latest"))?;
        let Some(tag) = location
            .as_deref()
            .and_then(|l| release::tag_from_redirect(l, repo))
            .filter(|tag| !publik || release::publik_tag(tag).is_some())
        else {
            // No published release: github.com sends us to the release list instead.
            return Ok(Answer { latest: LatestRelease::None, source: Source::GitHubPages });
        };
        if let Some(e) = self.entry(repo, false)
            && let Some(r) = e.release.as_ref().filter(|r| r.tag == tag)
        {
            return Ok(Answer { latest: LatestRelease::Found(r.clone()), source: Source::GitHubPages });
        }
        let sums_url = format!("{}{tag}/SHA256SUMS.txt", release::download_prefix(repo));
        let sums = self.http.get_text(&sums_url)?.map(|t| release::parse_sha256sums(&t)).unwrap_or_default();
        let mut r = release::release_from_sums(repo, &tag, &sums);
        if publik {
            r.version = release::publik_tag(&tag);
        }
        self.store(repo, Entry { etag: None, release: Some(r.clone()), prereleases: false, fetched_at: crate::now() });
        Ok(Answer { latest: LatestRelease::Found(r), source: Source::GitHubPages })
    }

    /// The API, else (when it's out of quota) the release pages, else the saved answer.
    fn lookup(&self, repo: &str, prereleases: bool, publik: bool) -> Answer {
        let api_error = if self.limited() {
            None
        } else {
            match self.ask_api(repo, prereleases, publik) {
                Ok(Some(answer)) => return answer,
                Ok(None) => None,
                Err(e) => Some(e),
            }
        };
        if !prereleases && api_error.is_none() {
            match self.ask_pages(repo, publik) {
                Ok(answer) => return answer,
                Err(e) => log::warn!("{repo}: release pages: {e}"),
            }
        }
        let why = api_error.unwrap_or_else(|| "GitHub's request limit was reached".into());
        match self.cached(repo, prereleases) {
            Some(latest) => Answer { latest, source: Source::Cache },
            None => Answer { latest: LatestRelease::Failed(why), source: Source::Cache },
        }
    }
}

impl ReleaseSource for GitHubSource {
    fn latest(&self, repo: &str, prereleases: bool) -> Answer {
        self.lookup(repo, prereleases, false)
    }

    fn publik(&self, repo: &str, prereleases: bool) -> Answer {
        self.lookup(repo, prereleases, true)
    }

    fn cached(&self, repo: &str, prereleases: bool) -> Option<LatestRelease> {
        self.entry(repo, prereleases).map(|e| e.release.map_or(LatestRelease::None, LatestRelease::Found))
    }

    fn rate_limited_until(&self) -> Option<i64> {
        let until = *self.limited_until.lock().unwrap_or_else(PoisonError::into_inner);
        until.filter(|u| *u > crate::now())
    }

    fn save(&self) {
        let json = {
            let cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
            serde_json::to_vec(&*cache)
        };
        match json {
            Ok(bytes) => {
                if let Err(e) = crate::paths::write_atomic(&self.cache_path, &bytes) {
                    log::warn!("release cache: {e}");
                }
            }
            Err(e) => log::warn!("release cache: {e}"),
        }
    }
}
