// SPDX-License-Identifier: GPL-3.0-or-later
//! Typed routes. Every screen is reached through a [`Route`]; `--route` parses the same
//! syntax ([`Route::parse`]).

use std::fmt;
use std::sync::Arc;

use opensmarters_store::ProfileId;

use crate::playback::PlaybackRequest;

/// A side-rail section (which nav item is highlighted).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    /// Search.
    Search,
    /// Home.
    Home,
    /// Live TV.
    Live,
    /// TV guide (EPG grid).
    Guide,
    /// Movies.
    Movies,
    /// Series.
    Series,
    /// Catch-up.
    Catchup,
    /// Favourites.
    Favourites,
    /// Settings.
    Settings,
}

/// Settings sub-pages (§8.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SettingsPage {
    /// Account status, expiry, connections.
    #[default]
    Account,
    /// Live container, timeshift format, User-Agent; languages, decoding, buffer.
    Playback,
    /// Accounts / profiles management.
    Profiles,
    /// Parental control (PIN, keywords, locks).
    Parental,
    /// TV mode, UI size, reduced motion.
    Appearance,
    /// Clock format, display language.
    Language,
    /// Cache, history, backup (export/import), data folder, logs.
    Storage,
    /// Privacy facts, optional TMDB.
    Privacy,
    /// Versions, licences.
    About,
}

impl SettingsPage {
    /// All pages in menu order.
    pub const ALL: [SettingsPage; 9] = [
        SettingsPage::Account,
        SettingsPage::Profiles,
        SettingsPage::Playback,
        SettingsPage::Parental,
        SettingsPage::Appearance,
        SettingsPage::Language,
        SettingsPage::Storage,
        SettingsPage::Privacy,
        SettingsPage::About,
    ];

    /// Menu label.
    pub fn label(self) -> &'static str {
        match self {
            SettingsPage::Account => "Account",
            SettingsPage::Playback => "Playback",
            SettingsPage::Profiles => "Profiles",
            SettingsPage::Parental => "Parental control",
            SettingsPage::Appearance => "Appearance",
            SettingsPage::Language => "Language & region",
            SettingsPage::Storage => "Storage & data",
            SettingsPage::Privacy => "Privacy & security",
            SettingsPage::About => "About",
        }
    }

    /// `--route settings:<slug>` name.
    pub fn slug(self) -> &'static str {
        match self {
            SettingsPage::Account => "account",
            SettingsPage::Playback => "playback",
            SettingsPage::Profiles => "profiles",
            SettingsPage::Parental => "parental",
            SettingsPage::Appearance => "appearance",
            SettingsPage::Language => "language",
            SettingsPage::Storage => "storage",
            SettingsPage::Privacy => "privacy",
            SettingsPage::About => "about",
        }
    }

    fn from_slug(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.slug() == s)
    }
}

/// How the player screen was asked to play something.
#[derive(Debug, Clone)]
pub enum PlayerRoute {
    /// A fully resolved request (URL built by the session).
    Request(Arc<PlaybackRequest>),
    /// A live channel by its key (Xtream `stream_id` or M3U id), resolved by the player
    /// screen from the live catalogue (used by `--route player:live=<id>`).
    LiveChannel {
        /// Channel key.
        key: String,
    },
}

/// A screen and its parameters.
#[derive(Debug, Clone)]
pub enum Route {
    /// First-run welcome (no profiles yet).
    Welcome,
    /// Profile picker ("Who's watching?").
    Profiles,
    /// Add (or edit) an account.
    Onboarding {
        /// Profile to edit; `None` adds a new one.
        edit: Option<ProfileId>,
    },
    /// Home.
    Home,
    /// Live TV, optionally opened on a category id (`"all"`, `"favourites"` or a provider id).
    Live {
        /// Category to select.
        category: Option<String>,
    },
    /// EPG grid.
    Guide,
    /// Movies browse.
    Movies {
        /// Category to select.
        category: Option<String>,
    },
    /// Series browse.
    Series {
        /// Category to select.
        category: Option<String>,
    },
    /// Movie detail page.
    MovieDetail {
        /// VOD `stream_id`.
        stream_id: u64,
    },
    /// Series detail page (seasons, episodes).
    SeriesDetail {
        /// `series_id`.
        series_id: u64,
    },
    /// Catch-up browse, optionally for one channel.
    Catchup {
        /// Live `stream_id`.
        stream_id: Option<u64>,
    },
    /// Search.
    Search {
        /// Initial query.
        query: String,
    },
    /// Favourites.
    Favourites,
    /// Settings.
    Settings {
        /// Selected page.
        page: SettingsPage,
    },
    /// Fullscreen player.
    Player(PlayerRoute),
}

impl Route {
    /// Live TV with no category preselected.
    pub fn live() -> Self {
        Route::Live { category: None }
    }

    /// Settings on the first page.
    pub fn settings() -> Self {
        Route::Settings {
            page: SettingsPage::default(),
        }
    }

    /// The side-rail section this route belongs to.
    pub fn section(&self) -> Option<Section> {
        Some(match self {
            Route::Welcome | Route::Profiles | Route::Onboarding { .. } | Route::Player(_) => {
                return None;
            }
            Route::Home => Section::Home,
            Route::Live { .. } => Section::Live,
            Route::Guide => Section::Guide,
            Route::Movies { .. } | Route::MovieDetail { .. } => Section::Movies,
            Route::Series { .. } | Route::SeriesDetail { .. } => Section::Series,
            Route::Catchup { .. } => Section::Catchup,
            Route::Search { .. } => Section::Search,
            Route::Favourites => Section::Favourites,
            Route::Settings { .. } => Section::Settings,
        })
    }

    /// True if the route needs an open session (everything but profiles/onboarding).
    pub fn needs_session(&self) -> bool {
        !matches!(
            self,
            Route::Welcome | Route::Profiles | Route::Onboarding { .. }
        )
    }

    /// True if two routes show the same page (pushing it again is a no-op). Player routes
    /// are never "the same".
    pub fn same_page(&self, other: &Route) -> bool {
        match (self, other) {
            (Route::Player(_), _) | (_, Route::Player(_)) => false,
            (Route::Onboarding { edit: a }, Route::Onboarding { edit: b }) => a == b,
            (Route::Live { category: a }, Route::Live { category: b })
            | (Route::Movies { category: a }, Route::Movies { category: b })
            | (Route::Series { category: a }, Route::Series { category: b }) => a == b,
            (Route::MovieDetail { stream_id: a }, Route::MovieDetail { stream_id: b }) => a == b,
            (Route::SeriesDetail { series_id: a }, Route::SeriesDetail { series_id: b }) => a == b,
            (Route::Catchup { stream_id: a }, Route::Catchup { stream_id: b }) => a == b,
            (Route::Search { query: a }, Route::Search { query: b }) => a == b,
            (Route::Settings { page: a }, Route::Settings { page: b }) => a == b,
            (a, b) => std::mem::discriminant(a) == std::mem::discriminant(b),
        }
    }

    /// Parses the `--route` syntax: `name[:key=value[,key=value]]`, e.g. `home`, `live`,
    /// `live:category=12`, `player:live=1001`, `settings:about`, `onboarding`,
    /// `detail:movie=200001`, `detail:series=7`, `catchup:stream=1001`, `search:q=news`.
    ///
    /// # Errors
    ///
    /// A human-readable message for unknown names or malformed parameters.
    pub fn parse(s: &str) -> Result<Route, String> {
        let s = s.trim();
        let (name, params) = s.split_once(':').unwrap_or((s, ""));
        let mut kv: Vec<(&str, &str)> = Vec::new();
        let mut bare: Option<&str> = None;
        for part in params.split(',').filter(|p| !p.is_empty()) {
            match part.split_once('=') {
                Some((k, v)) => kv.push((k.trim(), v.trim())),
                None => bare = Some(part.trim()),
            }
        }
        let get = |k: &str| kv.iter().find(|(key, _)| *key == k).map(|(_, v)| *v);
        let num = |k: &str| -> Result<Option<u64>, String> {
            get(k)
                .map(|v| {
                    v.parse::<u64>()
                        .map_err(|_| format!("{k} must be a number"))
                })
                .transpose()
        };
        let route = match name.to_ascii_lowercase().as_str() {
            "welcome" => Route::Welcome,
            "profiles" => Route::Profiles,
            "onboarding" | "login" | "add-account" => Route::Onboarding { edit: None },
            "home" => Route::Home,
            "live" => Route::Live {
                category: get("category").map(str::to_owned),
            },
            "guide" | "epg" => Route::Guide,
            "movies" => Route::Movies {
                category: get("category").map(str::to_owned),
            },
            "series" => Route::Series {
                category: get("category").map(str::to_owned),
            },
            "detail" => {
                if let Some(id) = num("movie")? {
                    Route::MovieDetail { stream_id: id }
                } else if let Some(id) = num("series")? {
                    Route::SeriesDetail { series_id: id }
                } else {
                    return Err("detail needs movie=<id> or series=<id>".into());
                }
            }
            "catchup" => Route::Catchup {
                stream_id: num("stream")?,
            },
            "search" => Route::Search {
                query: get("q").unwrap_or_default().to_owned(),
            },
            "favourites" | "favorites" => Route::Favourites,
            "settings" => {
                let page = match bare.or_else(|| get("page")) {
                    Some(slug) => SettingsPage::from_slug(slug)
                        .ok_or_else(|| format!("unknown settings page {slug:?}"))?,
                    None => SettingsPage::default(),
                };
                Route::Settings { page }
            }
            "player" => match get("live") {
                Some(key) if !key.is_empty() => Route::Player(PlayerRoute::LiveChannel {
                    key: key.to_owned(),
                }),
                _ => return Err("player needs live=<channel id>".into()),
            },
            other => return Err(format!("unknown route {other:?}")),
        };
        Ok(route)
    }
}

impl fmt::Display for Route {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Route::Welcome => f.write_str("welcome"),
            Route::Profiles => f.write_str("profiles"),
            Route::Onboarding { edit: None } => f.write_str("onboarding"),
            Route::Onboarding { edit: Some(id) } => write!(f, "onboarding:edit={id:?}"),
            Route::Home => f.write_str("home"),
            Route::Live { category: None } => f.write_str("live"),
            Route::Live { category: Some(c) } => write!(f, "live:category={c}"),
            Route::Guide => f.write_str("guide"),
            Route::Movies { category: None } => f.write_str("movies"),
            Route::Movies { category: Some(c) } => write!(f, "movies:category={c}"),
            Route::Series { category: None } => f.write_str("series"),
            Route::Series { category: Some(c) } => write!(f, "series:category={c}"),
            Route::MovieDetail { stream_id } => write!(f, "detail:movie={stream_id}"),
            Route::SeriesDetail { series_id } => write!(f, "detail:series={series_id}"),
            Route::Catchup { stream_id: None } => f.write_str("catchup"),
            Route::Catchup { stream_id: Some(s) } => write!(f, "catchup:stream={s}"),
            Route::Search { query } => write!(f, "search:q={query}"),
            Route::Favourites => f.write_str("favourites"),
            Route::Settings { page } => write!(f, "settings:{}", page.slug()),
            Route::Player(PlayerRoute::LiveChannel { key }) => write!(f, "player:live={key}"),
            Route::Player(PlayerRoute::Request(r)) => write!(f, "player:{:?}", r.kind),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cli_routes() {
        assert!(matches!(Route::parse("home"), Ok(Route::Home)));
        assert!(matches!(
            Route::parse("live:category=12"),
            Ok(Route::Live { category: Some(c) }) if c == "12"
        ));
        assert!(matches!(
            Route::parse("player:live=1001"),
            Ok(Route::Player(PlayerRoute::LiveChannel { key })) if key == "1001"
        ));
        assert!(matches!(
            Route::parse("settings:about"),
            Ok(Route::Settings {
                page: SettingsPage::About
            })
        ));
        assert!(matches!(
            Route::parse("detail:movie=200001"),
            Ok(Route::MovieDetail { stream_id: 200_001 })
        ));
        assert!(Route::parse("detail:movie=abc").is_err());
        assert!(Route::parse("nope").is_err());
        assert!(Route::parse("player").is_err());
    }

    #[test]
    fn display_round_trips() {
        for s in [
            "welcome",
            "home",
            "live",
            "live:category=7",
            "settings:appearance",
            "settings:language",
            "settings:privacy",
            "settings:storage",
            "detail:series=3",
            "player:live=1001",
            "catchup:stream=5",
        ] {
            let r = Route::parse(s).unwrap_or(Route::Profiles);
            assert_eq!(r.to_string(), s);
        }
    }

    #[test]
    fn same_page_semantics() {
        assert!(Route::Home.same_page(&Route::Home));
        assert!(!Route::live().same_page(&Route::Live {
            category: Some("1".into())
        }));
        let p = Route::Player(PlayerRoute::LiveChannel { key: "1".into() });
        assert!(!p.same_page(&p.clone()));
        assert_eq!(Route::Guide.section(), Some(Section::Guide));
        assert_eq!(Route::Profiles.section(), None);
    }
}
