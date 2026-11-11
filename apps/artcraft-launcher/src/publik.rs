//! What publik needs to know about this build, and the gate every publik app opens with
//! (publikhq.com/skills/setup.md, the module at publikhq.com/sdk/publik.rs.md, shared with the
//! other Craft apps as the `craft-publik` crate).
//!
//! `Launcher::new` creates `craft_publik::ui::Gate::new(app_info())`, which runs
//! `craft_publik::gate::ensure_signed_in` on a worker thread, and `Shell::ui` draws it before
//! anything of the launcher: Sign in with publik (`POST /api/v1/installs` mints this computer's
//! key; `GET /api/v1/wallet` says when the person signed in). The launcher calls no model, so the
//! gate never asks how to pay for AI. Usage counting (`POST /api/v1/telemetry`: `app_opened`,
//! then `active_session` every 30 minutes, with the install key) starts when the gate passed.
//! `PUBLIK_SELF_TEST=1` prints `publik self-test: ok craft-launcher <version>` instead.

/// The publik listing: publikhq.com/craft-launcher. Also the credential file's name.
pub const APP_SLUG: &str = "craft-launcher";
pub const APP_NAME: &str = "ArtCraft Launcher";
/// The launcher installs, updates and opens the apps; it calls no model.
pub const USES_AI: bool = false;
/// The publik paths and events this app calls (the module's contract; publik's scan reads them here).
pub const ENDPOINTS: craft_publik::Endpoints = craft_publik::Endpoints {
    installs: "/api/v1/installs",
    wallet: "/api/v1/wallet",
    telemetry: "/api/v1/telemetry",
    opened: "app_opened",
    active: "active_session",
};

/// The version publik counts and the listing shows: the release tag without its `v`, for example
/// `0.16.1-publik.7`. The build number comes from `packaging/PUBLIK_BUILD`, the file the bundle
/// version comes from.
pub fn version() -> &'static str {
    static VERSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    VERSION.get_or_init(|| format!("{}-publik.{}", env!("CARGO_PKG_VERSION"), include_str!("../../../packaging/PUBLIK_BUILD").trim()))
}

/// What publik needs to know about this build. The app token comes from `build.rs`
/// (`PUBLIK_APP_TOKEN`, or `publik-app-token.txt` at the repo root); it is public by design and
/// can only mint capped keys attributed to the launcher. The launcher keeps no AI settings, so
/// there is no config directory.
pub fn app_info() -> craft_publik::AppInfo {
    craft_publik::AppInfo {
        slug: APP_SLUG,
        name: APP_NAME,
        version: version(),
        app_token: include_str!(concat!(env!("OUT_DIR"), "/publik-app-token.txt")),
        uses_ai: USES_AI,
        endpoints: ENDPOINTS,
        dialects: &[],
        config_dir: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_build_names_itself_to_publik() {
        let info = app_info();
        assert_eq!(info.slug, "craft-launcher");
        assert!(!info.uses_ai);
        assert_eq!(info.endpoints, craft_publik::Endpoints::DEFAULT);
        assert!(info.dialects.is_empty());
        assert!(version().starts_with(env!("CARGO_PKG_VERSION")));
        assert!(version().contains("-publik."));
        assert!(!version().starts_with('v'));
    }
}
