// SPDX-License-Identifier: GPL-3.0-or-later
//! Async Xtream Codes API client.
//!
//! All calls are `GET {server}/player_api.php?username=…&password=…[&action=…]` with
//! **every query value percent-encoded** (form encoding, as PHP's `$_GET` expects).
//! Responses are parsed leniently (see [`crate::de`]); a `text/html` content type is
//! fine as long as the body is JSON.
//!
//! | Method | Action | Notes |
//! |---|---|---|
//! | [`Client::login`] | *(none)* | auth + `Active` + expiry vs server clock, else [`Error::Auth`] |
//! | [`Client::account_info`] | *(none)* | same request, no checks |
//! | [`Client::live_categories`] / [`vod_categories`](Client::vod_categories) / [`series_categories`](Client::series_categories) | `get_*_categories` | |
//! | [`Client::live_streams`] / [`vod_streams`](Client::vod_streams) / [`series`](Client::series) | `get_live_streams` / `get_vod_streams` / `get_series` | `None` = All: **`category_id` omitted** |
//! | [`Client::vod_info`] | `get_vod_info&vod_id=` | `[]` → default |
//! | [`Client::series_info`] | `get_series_info&series_id=` | episodes normalised |
//! | [`Client::short_epg`] | `get_short_epg&stream_id=[&limit=]` | base64 decoded |
//! | [`Client::simple_data_table`] | `get_simple_data_table&stream_id=` | full EPG + catch-up |
//! | [`Client::xmltv_url`] | — | `xmltv.php` URL (download with [`crate::net::HttpClient`] and parse with [`crate::xmltv`]) |
//!
//! Calls are not retried and nothing is cached here; the caller decides.

use std::sync::{PoisonError, RwLock};

use chrono::Utc;
use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;
use url::Url;

use crate::de::{self, Listing};
use crate::epg::ServerTimeZone;
use crate::error::Error;
use crate::model::{
    Category, CategoryId, EpgListing, LiveStream, LoginInfo, Series, SeriesInfo, VodInfo,
    VodStream, XtreamAccount, parse_epg_listings,
};
use crate::net::{HttpClient, HttpConfig};
use crate::redact::Redactor;
use crate::urls::StreamUrls;

/// An Xtream Codes API client for one account.
///
/// Cheap to share behind an `Arc`; all methods take `&self`. `Debug` never prints the
/// password.
pub struct Client {
    http: HttpClient,
    account: XtreamAccount,
    password: SecretString,
    time_zone: RwLock<ServerTimeZone>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("account", &self.account)
            .field("password", &"***")
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Creates a client with its own [`Redactor`] (which knows the username and
    /// password).
    ///
    /// # Errors
    ///
    /// See [`HttpClient::new`].
    pub fn new(
        account: XtreamAccount,
        password: SecretString,
        config: &HttpConfig,
    ) -> Result<Self, Error> {
        Self::with_redactor(account, password, config, Redactor::new())
    }

    /// Creates a client that registers its credentials with a shared `redactor`
    /// (typically the app-wide one used by the logging layer).
    ///
    /// # Errors
    ///
    /// See [`HttpClient::new`].
    pub fn with_redactor(
        account: XtreamAccount,
        password: SecretString,
        config: &HttpConfig,
        redactor: Redactor,
    ) -> Result<Self, Error> {
        redactor.add_secret(password.expose_secret());
        redactor.add_secret(account.username());
        Ok(Client {
            http: HttpClient::new(config, redactor)?,
            account,
            password,
            time_zone: RwLock::new(ServerTimeZone::Utc),
        })
    }

    /// The account.
    pub fn account(&self) -> &XtreamAccount {
        &self.account
    }

    /// The redactor that knows this client's credentials. Run every log line that may
    /// contain a URL through it.
    pub fn redactor(&self) -> &Redactor {
        self.http.redactor()
    }

    /// The underlying capped HTTP client (same User-Agent, timeouts, size cap and
    /// redactor), e.g. to download [`Client::xmltv_url`].
    pub fn http(&self) -> &HttpClient {
        &self.http
    }

    /// The server time zone used to convert EPG local strings. Set by [`Client::login`]
    /// / [`Client::account_info`] from `server_info`; UTC until then.
    pub fn server_time_zone(&self) -> ServerTimeZone {
        *self
            .time_zone
            .read()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Overrides the server time zone (e.g. restored from a cached login).
    pub fn set_server_time_zone(&self, tz: ServerTimeZone) {
        *self
            .time_zone
            .write()
            .unwrap_or_else(PoisonError::into_inner) = tz;
    }

    /// Stream URL builder for this account, based on a login response.
    pub fn stream_urls(&self, login: &LoginInfo) -> StreamUrls {
        StreamUrls::new(login, &self.account, self.password.clone())
    }

    /// `{server}/xmltv.php?username=…&password=…`. **Contains the password.**
    pub fn xmltv_url(&self) -> Url {
        let mut url = self.account.server().endpoint("xmltv.php");
        url.query_pairs_mut()
            .append_pair("username", self.account.username())
            .append_pair("password", self.password.expose_secret());
        url
    }

    /// `player_api.php` URL with credentials and the given extra query pairs.
    fn api_url(&self, params: &[(&str, &str)]) -> Url {
        let mut url = self.account.server().endpoint("player_api.php");
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("username", self.account.username())
                .append_pair("password", self.password.expose_secret());
            for (k, v) in params {
                q.append_pair(k, v);
            }
        }
        url
    }

    async fn fetch(&self, params: &[(&str, &str)]) -> Result<Vec<u8>, Error> {
        self.http.get_bytes(&self.api_url(params)).await
    }

    /// Logs in: fetches the account info and applies [`LoginInfo::check`] (auth truthy,
    /// status `Active`, `exp_date` after the server clock). Also records the server
    /// time zone.
    ///
    /// # Errors
    ///
    /// [`Error::Auth`] with the reason (invalid credentials / status / expired), plus the
    /// transport and parse errors of [`Client::account_info`].
    pub async fn login(&self) -> Result<LoginInfo, Error> {
        let info = self.account_info().await?;
        info.check(Utc::now())
            .map_err(|status| Error::Auth { status })?;
        Ok(info)
    }

    /// Fetches the login response **without** checking it (e.g. to show the account page
    /// of an expired account). Records the server time zone.
    ///
    /// # Errors
    ///
    /// Transport errors and [`Error::Parse`] for non-JSON bodies.
    pub async fn account_info(&self) -> Result<LoginInfo, Error> {
        let body = self.fetch(&[]).await?;
        let info = LoginInfo::from_json(&body)?;
        self.set_server_time_zone(info.time_zone());
        Ok(info)
    }

    async fn list<T: DeserializeOwned>(
        &self,
        action: &str,
        category: Option<&CategoryId>,
    ) -> Result<Listing<T>, Error> {
        let mut params = vec![("action", action)];
        if let Some(cat) = category {
            params.push(("category_id", cat.as_str()));
        }
        let body = self.fetch(&params).await?;
        let listing = de::parse_list::<T>(&body)?;
        report_skipped(action, &listing);
        Ok(listing)
    }

    /// `get_live_categories`.
    ///
    /// # Errors
    ///
    /// Transport errors, [`Error::Parse`], [`Error::Auth`].
    pub async fn live_categories(&self) -> Result<Listing<Category>, Error> {
        self.list("get_live_categories", None).await
    }

    /// `get_vod_categories`.
    ///
    /// # Errors
    ///
    /// Transport errors, [`Error::Parse`], [`Error::Auth`].
    pub async fn vod_categories(&self) -> Result<Listing<Category>, Error> {
        self.list("get_vod_categories", None).await
    }

    /// `get_series_categories`.
    ///
    /// # Errors
    ///
    /// Transport errors, [`Error::Parse`], [`Error::Auth`].
    pub async fn series_categories(&self) -> Result<Listing<Category>, Error> {
        self.list("get_series_categories", None).await
    }

    /// `get_live_streams`, optionally for one category. `None` means *All* and omits
    /// `category_id` (Favourites are filtered locally; see [`crate::catalog`]).
    ///
    /// # Errors
    ///
    /// Transport errors, [`Error::Parse`], [`Error::Auth`].
    pub async fn live_streams(
        &self,
        category: Option<&CategoryId>,
    ) -> Result<Listing<LiveStream>, Error> {
        self.list("get_live_streams", category).await
    }

    /// `get_vod_streams`; `None` = All.
    ///
    /// # Errors
    ///
    /// Transport errors, [`Error::Parse`], [`Error::Auth`].
    pub async fn vod_streams(
        &self,
        category: Option<&CategoryId>,
    ) -> Result<Listing<VodStream>, Error> {
        self.list("get_vod_streams", category).await
    }

    /// `get_series`; `None` = All.
    ///
    /// # Errors
    ///
    /// Transport errors, [`Error::Parse`], [`Error::Auth`].
    pub async fn series(&self, category: Option<&CategoryId>) -> Result<Listing<Series>, Error> {
        self.list("get_series", category).await
    }

    /// `get_vod_info&vod_id=…`. An unknown id (`[]`) yields `VodInfo::default()`.
    ///
    /// # Errors
    ///
    /// Transport errors, [`Error::Parse`], [`Error::Auth`].
    pub async fn vod_info(&self, vod_id: u64) -> Result<VodInfo, Error> {
        let id = vod_id.to_string();
        let body = self
            .fetch(&[("action", "get_vod_info"), ("vod_id", &id)])
            .await?;
        de::parse_object(&body)
    }

    /// `get_series_info&series_id=…`, with episodes normalised by season.
    ///
    /// # Errors
    ///
    /// Transport errors, [`Error::Parse`], [`Error::Auth`].
    pub async fn series_info(&self, series_id: u64) -> Result<SeriesInfo, Error> {
        let id = series_id.to_string();
        let body = self
            .fetch(&[("action", "get_series_info"), ("series_id", &id)])
            .await?;
        de::parse_object(&body)
    }

    /// `get_short_epg&stream_id=…[&limit=…]` (the original sends no limit).
    ///
    /// # Errors
    ///
    /// Transport errors, [`Error::Parse`], [`Error::Auth`].
    pub async fn short_epg(
        &self,
        stream_id: u64,
        limit: Option<u32>,
    ) -> Result<Listing<EpgListing>, Error> {
        let id = stream_id.to_string();
        let limit = limit.map(|l| l.to_string());
        let mut params = vec![("action", "get_short_epg"), ("stream_id", id.as_str())];
        if let Some(l) = &limit {
            params.push(("limit", l.as_str()));
        }
        let body = self.fetch(&params).await?;
        let listing = parse_epg_listings(&body, &self.server_time_zone())?;
        report_skipped("get_short_epg", &listing);
        Ok(listing)
    }

    /// `get_simple_data_table&stream_id=…`: the full EPG of a channel, including
    /// `has_archive` flags for catch-up (see [`crate::model::archive_programmes`]).
    ///
    /// # Errors
    ///
    /// Transport errors, [`Error::Parse`], [`Error::Auth`].
    pub async fn simple_data_table(&self, stream_id: u64) -> Result<Listing<EpgListing>, Error> {
        let id = stream_id.to_string();
        let body = self
            .fetch(&[("action", "get_simple_data_table"), ("stream_id", &id)])
            .await?;
        let listing = parse_epg_listings(&body, &self.server_time_zone())?;
        report_skipped("get_simple_data_table", &listing);
        Ok(listing)
    }
}

fn report_skipped<T>(action: &str, listing: &Listing<T>) {
    if listing.skipped > 0 {
        tracing::warn!(
            action,
            kept = listing.items.len(),
            skipped = listing.skipped,
            first_error = listing.errors.first().map(|e| e.message.as_str()),
            "skipped malformed items"
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn client(password: &str) -> Client {
        let account = XtreamAccount::new("panel.example.com:8080/", "demo user").unwrap();
        Client::new(
            account,
            SecretString::from(password.to_owned()),
            &HttpConfig::default(),
        )
        .unwrap()
    }

    #[test]
    fn api_urls_encode_every_value() {
        let c = client("p@ss w/rd&x=1#?%é");
        let url = c.api_url(&[("action", "get_live_streams"), ("category_id", "a&b")]);
        assert_eq!(
            url.as_str(),
            "http://panel.example.com:8080/player_api.php?username=demo+user&password=p%40ss+w%2Frd%26x%3D1%23%3F%25%C3%A9&action=get_live_streams&category_id=a%26b"
        );
        let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
        assert_eq!(pairs[1], ("password".into(), "p@ss w/rd&x=1#?%é".into()));
    }

    #[test]
    fn xmltv_url() {
        let c = client("pw");
        assert_eq!(
            c.xmltv_url().as_str(),
            "http://panel.example.com:8080/xmltv.php?username=demo+user&password=pw"
        );
    }

    #[test]
    fn debug_hides_password_and_redactor_knows_it() {
        let c = client("hunter2-secret");
        assert!(!format!("{c:?}").contains("hunter2"));
        assert_eq!(c.redactor().redact("x hunter2-secret y"), "x *** y");
    }

    /// The app spawns these on a multi-threaded runtime: the client must be shareable
    /// and its futures `Send`.
    #[test]
    fn client_and_futures_are_send_and_sync() {
        fn send_sync<T: Send + Sync>(_: &T) {}
        fn send<T: Send>(_: T) {}
        let c = client("pw");
        send_sync(&c);
        send(c.login());
        send(c.account_info());
        send(c.live_categories());
        send(c.live_streams(None));
        send(c.vod_info(1));
        send(c.series_info(1));
        send(c.short_epg(1, Some(4)));
        send(c.simple_data_table(1));
        send(c.http().get_bytes(&c.xmltv_url()));
    }
}
