# Wire protocol used by IPTV Smarters Pro (Windows 1.1.2)

This describes what the original client sends and which response fields it actually reads. It is meant to be the compatibility spec for opensmarters.

- Citations are relative to the unpacked `app.asar` root (`www/js/main.js:1999`, …).
- **Faithful** means what the original does.
- **Recommended** marks places where opensmarters should do better without breaking compatibility.

---

## Transport

| Aspect | Original behaviour |
|---|---|
| Client | jQuery 1.11.3 `$.ajax`. Always `GET` with empty `data`, except one `HEAD` (the M3U pre-check). No custom headers, `dataType`, `cache:false` or cache-buster. |
| User-Agent (API, images) | `IPTVSmartersPro`, set for the whole window (`www/app.js:39-45`). |
| User-Agent (streams) | libVLC 2.2 default. No `:http-user-agent` and no per-item headers are passed. |
| JSON | `if (typeof data == "string") data = JSON.parse(data)`, so a `text/html` content type is tolerated. A non-JSON body throws, uncaught. |
| Timeouts | Xtream API and TMDB: 80 s. M3U download: 180 s. The M3U `HEAD` check and the multi-DNS probes use the jQuery default (none). |
| Encoding | **Nothing is URL-encoded.** Username, password, ids and the TMDB query are concatenated raw. |
| Retries | M3U download only (5 retries, about 4 s apart). Xtream API calls are not retried. |
| Caching | None. Category and stream lists are fetched again on every section entry, and VOD and series info on every open. Only the login response is persisted. |

**Recommended:**
- Percent-encode every query value and path segment.
- Send `User-Agent: IPTVSmartersPro`, or make it configurable, for **both** API and streams. Some panels whitelist known player UAs.
- Cache category and stream lists per account with a TTL.

---

## Xtream Codes API

`{login}` is the server URL exactly as the user typed it. The only normalisation is to prepend `http://` when the string contains no `http` (`www/js/keyHandler.js:6780-6781`). Trailing slashes are not stripped and no default port is added.

All calls go to `{login}/player_api.php?username={u}&password={p}[&action=…]`.

| Purpose | Extra query | Caller |
|---|---|---|
| Login | *(none)* | `www/js/main.js:1998-2114` |
| Categories | `action=get_live_categories` \| `get_vod_categories` \| `get_series_categories` | `www/js/main.js:2120-2189` |
| Live streams | `action=get_live_streams&category_id={id}` | `www/js/main.js:3273-3310` (also used for the catch-up and multiscreen lists) |
| VOD list | `action=get_vod_streams&category_id={id}` | `www/js/main.js:2746-2800` |
| Series list | `action=get_series&category_id={id}` | same |
| VOD info | `action=get_vod_info&vod_id={stream_id}` | `www/js/main.js:2671-2695` |
| Series info | `action=get_series_info&series_id={series_id}` | `www/js/main.js:2981-3005` |
| Short EPG | `action=get_short_epg&stream_id={id}` (no `limit`) | `www/js/main.js:3607-3627` |
| Full EPG and catch-up archive | `action=get_simple_data_table&stream_id={id}` | `www/js/main.js:2193-2398`, `2405-2576` |

Never used: `panel_api.php`, `xmltv.php`, `get.php` (except as an opaque M3U URL), Stalker portal, Enigma2.

When a category is entered, the category action maps to its stream action: `get_live_categories → get_live_streams`, `get_vod_categories → get_vod_streams`, `get_series_categories → get_series` (`www/js/keyHandler.js:4597-4599`).

### Pseudo-categories
- The client prepends `{category_id:"ALL"}` to every category list.
- It also prepends `{category_id:"FAVOURITE"}`, except in catch-up and multiscreen (`www/js/main.js:2135-2141`).
- Both values are **sent to the server literally** (`&category_id=ALL`, `&category_id=FAVOURITE`). The app relies on the panel ignoring a non-numeric id and returning everything. FAVOURITE is then filtered client-side against the stored ids.
- **Recommended:** omit `category_id` for "All" and filter favourites locally.

### Login
Success requires `user_info.auth` to be truthy **and** `user_info.status === "Active"`, an exact and case-sensitive comparison (`www/js/main.js:2030-2031`).

| Condition | Message shown |
|---|---|
| `auth` truthy, other status | "Your Status is {status}" (Expired, Banned, Disabled, …) |
| `auth` falsy | "Invalid Username or Password" |
| HTTP error or timeout | "Invalid Details!" |
| `navigator.onLine === false` | "Trouble connecting to Internet…" |

The **entire response** is persisted as the profile, with two client fields added: `user_info.name` (the "Any Name" label) and `user_info.login_url`.

Expected shape, from a commented sample at `www/js/main.js:2009` (values elided):

```jsonc
{
  "user_info": {
    "username": "", "password": "", "message": "", "auth": 1, "status": "Active",
    "exp_date": "<epoch s | null>", "is_trial": "0", "active_cons": "0",
    "created_at": "<epoch s>", "max_connections": "1",
    "allowed_output_formats": ["m3u8", "ts", "rtmp"]
  },
  "server_info": {
    "url": "host", "port": "80", "https_port": "8080", "server_protocol": "http",
    "rtmp_port": "", "timezone": "Europe/…", "timestamp_now": 0, "time_now": "YYYY-MM-DD HH:MM:SS"
  }
}
```

Fields read after login:

| Field | Used for |
|---|---|
| `server_info.{url, port, https_port, server_protocol}` | Stream URLs |
| `exp_date` | Header and account page, ×1000; falsy shows "Unlimited" |
| `is_trial`, `active_cons`, `created_at`, `max_connections`, `status` | Account page |
| `max_connections` | Multiscreen tile limit, enforced on the client |
| `allowed_output_formats`, `timezone`, `timestamp_now` | **Never used** |

**Recommended:**
- Check `exp_date` against the server clock.
- Offer only the containers listed in `allowed_output_formats`.
- Use `timezone`/`timestamp_now` to correct EPG times.
- Re-validate saved accounts at startup. The original does not.

### Fields read from list responses

**Categories**
- `category_id`, `category_name`, `parent_id`.

**Live streams**
- Read: `num`, `name`, `stream_id`, `stream_icon`, `epg_channel_id`, `added`, `category_id`, `tv_archive`, `tv_archive_duration`, `custom_sid`, `direct_source`, `stream_type`.
- Catch-up lists keep only entries where `tv_archive` is truthy.
- `tv_archive_duration` is copied but never used.

**VOD list**
- `stream_id`, `name`, `stream_icon`, `rating` (0–10, shown as `ceil(rating/2)` stars out of 5), `rating_5based`, `added`, `container_extension`.

**Series list**
- `series_id`, `name`, `cover`, `rating`, `rating_5based`, `last_modified`, `backdrop_path[]`, `plot`, `cast`, `director`, `genre`, `releaseDate` (camelCase), `youtube_trailer`.
- An item counts as a series when it has a `series_id`.
- The poster is `cover`, falling back to `stream_icon`.

**`get_vod_info`**
- Reads `info.{backdrop_path[], movie_image, rating, director, releasedate, duration, cast, genre, plot, youtube_trailer, tmdb_id}`.
- `movie_data` is **ignored**. The playback extension comes from the list item's `container_extension`.

**`get_series_info`**
- Only `episodes` is used: an object keyed by season number, each value an array of `{id, title, container_extension, info:{movie_image, rating, duration, plot}}`.
- The detail header is taken from the **list item**, not from `info`.
- `seasons[]` is ignored.

### Client-side sorting

| Option | Field and order |
|---|---|
| `atoz` / `ztoa` | `name`, case-insensitive |
| `topadded` | `added` descending (live, VOD) or `last_modified` descending (series) |
| `toprated` | `rating_5based` descending (VOD, series) |
| `channelnum_asc` / `channelnum_desc` | `num` (live) |
| `default` | Server order |

The original compares numeric strings as strings, so `"10" < "9"`. **Recommended:** compare numbers numerically.

---

## Stream URLs

```
base      = server_protocol == "https"
              ? "https://" + server_info.url + ":" + server_info.https_port
              : "http://"  + server_info.url + ":" + server_info.port

LIVE      = {base}/live/{username}/{password}/{stream_id}{container}          container ∈ {".ts", ".m3u8"} (setting, default ".ts")
MOVIE     = {base}/movie/{username}/{password}/{stream_id}.{container_extension}
EPISODE   = {base}/series/{username}/{password}/{episode.id}.{episode.container_extension}
TIMESHIFT = {base}/timeshift/{username}/{password}/{durationMin}/{start}/{stream_id}{container}
M3U item  = the item's url, verbatim
```

Sources: live `www/js/main.js:1355-1361`; movie `www/js/keyHandler.js:5619-5623`; episode `www/js/keyHandler.js:5183-5187`; timeshift `www/js/keyHandler.js:4488-4500`.

- **Streams use `server_info`, not the typed login URL.** The two can be different hosts. The port is always explicit, even 80. The original has no fallback when `server_info.url` is empty. **Recommended:** fall back to the host and port of `login_url`.
- The container setting applies only to **live and timeshift**. VOD and episodes always use their own `container_extension`.

### Catch-up / timeshift
- **`start`:** the programme's raw `start` string from `get_simple_data_table`, in the form `YYYY-MM-DD HH:MM:SS` in server-local time. Only the **first space** is replaced with `:`, so what is sent is **`YYYY-MM-DD:HH:MM:SS`**.
- **`durationMin`:** `(Date(end) − Date(start)) / 60000`. Both are parsed as *local* time and the result is not rounded, so it can be fractional and can be off across a DST change.
- **Interop note:** most other clients send `YYYY-MM-DD:HH-MM` with an integer number of minutes. **Recommended:** default to `YYYY-MM-DD:HH-MM` with `ceil(minutes)`, and offer a per-account fallback to the Smarters format. Verify both against real panels.
- Only programmes with `has_archive` truthy are offered. The window is not limited by `tv_archive_duration`.

---

## EPG

- **Sources:** `get_short_epg` (the 4-row side panel) and `get_simple_data_table` (the full-EPG popup and the catch-up lists). **No XMLTV.**
- **Fields read:** `epg_listings[].{id, epg_id, title, lang, start, end, description, channel_id, start_timestamp, stop_timestamp, now_playing, has_archive}`.
- **Decoding:** `title` and `description` are **base64 of UTF-8**, decoded as `decodeURIComponent(escape-bytes(atob(s)))` (`www/js/main.js:4115-4120`). The original has no try/catch.
- **Short EPG:** shows the first 4 entries in server order. `now_playing` is not checked.
- **Full EPG:** skips entries until the first one with `now_playing` truthy, then lists the rest.
- **Catch-up list:** groups programmes by the date part of `start`. Tabs are labelled `DD MMM YYYY`, and the newest date is the active tab.
- **Times:**
  - The display uses `start.substr(11,5)` (`HH:MM`), with an optional 12h format. **No timezone conversion is done**, so times are the server's wall clock.
  - The `*_timestamp` fields are ignored.
  - **Recommended:** use `start_timestamp`/`stop_timestamp` (epoch seconds) and convert to local time.

---

## M3U

### Loading
- **URL**
  1. Prepend `http://` if there is no scheme.
  2. Send a `HEAD` pre-check. If it fails, the app reports "Please provide valid login credentials", which breaks servers that reject `HEAD`.
  3. `GET` the playlist with a progress bar, a 180 s timeout and 5 retries.
- **File:** an Electron open dialog filtered to `*.m3u`, then `fs.readFile(path, 'utf-8')`.
- **No `get.php` → Xtream upgrade.** A `get.php?username=…&password=…&type=m3u_plus` URL is treated as an opaque playlist. **Recommended:** detect it and offer an Xtream login instead, which unlocks VOD, series, EPG and catch-up.
- **When the playlist is fetched:** it is downloaded again lazily the first time Live is opened in a session, then kept in memory.

### Parser (faithful description of `www/js/main.js:4438-4808`)
1. Accept the body if it contains `#EXTINF`. `#EXTM3U` is not required, and its `url-tvg`/`x-tvg-url` attributes are ignored.
2. Split the **whole text** on the literal `#EXTINF`. It is not split by lines, so a chunk includes any `#EXTGRP`, `#EXTVLCOPT` and URL lines up to the next entry.
3. Parse attributes by splitting on `"` and taking the segment that follows a key:
   - `tvg-id` / `tvg-ID` becomes `epgChannelId`.
   - `tvg-logo` becomes `stream_icon`.
   - `group-title` becomes `category_id = base64(group)`. An empty group means `"ALL"`.
   - `tvg-name` is read and then overwritten.
   - **Not parsed:** `tvg-chno`, `tvg-shift`, `catchup*`, `tvg-rec`, `#EXTGRP`, `#EXTVLCOPT`, `#KODIPROP`.
4. Take the name as the text after `",` up to the last `http://` (or the last `https://`). `\r` is not stripped.
5. Take the URL as everything from the last `http://` (preferred) or `https://` to the end of the chunk, with CR and LF removed.
6. Build the item `{name, num, url, stream_icon, epgChannelId, category_id}`. `num` starts at 2 when a header is present.
7. Build categories as unique groups in order of first appearance; numeric group names sort to the front. Prepend ALL and FAVOURITE.
8. Treat every item as a **live channel**. Nothing distinguishes movies or series, and there is no EPG in M3U mode.
9. Apply the white-label lock: the playlist is rejected unless more than 90% of URLs contain `Main.serverURL`. It is empty in this build, so the check always passes.

**Known parser defects (do not copy them):**
- An `http://` logo combined with an `https://` stream corrupts both the name and the URL.
- `rtmp`, `udp` and `rtsp` streams are dropped or get the previous item's URL, because `url` is a leaking global.
- Names end with a stray `\r` in CRLF files.
- `#EXTVLCOPT` lines end up in names.
- An attribute value that contains a key name is misread.

**Recommended parser:**
- Read line by line.
- Parse `#EXTINF:<dur> <k="v" …>,<title>` with a real attribute tokenizer.
- Honour `#EXTGRP` and `#EXTVLCOPT:http-user-agent` / `http-referrer`.
- Read `tvg-chno`, `catchup`, `catchup-days` and `catchup-source`.
- Read `url-tvg` from the header for XMLTV.
- Accept any URI scheme.
- Classify `/movie/` and `/series/` paths, or `.mkv`/`.mp4` endings, as VOD.

---

## TMDB (optional enrichment)

The original ships a **hard-coded v3 API key**. It reads `credits.cast[].{id, name, gender, profile_path}`, and for people `{name, birthday, place_of_birth, known_for_department, deathday, biography, images.profiles[].file_path}`.

| Trigger | Request |
|---|---|
| VOD detail, if `tmdb_id` is present | `GET https://api.themoviedb.org/3/movie/{tmdb_id}?api_key=K&append_to_response=credits` |
| Series detail, every time | `GET https://api.themoviedb.org/3/search/tv?api_key=K&query={name}` (not encoded) |
| Then, on an exact case-sensitive name match | `GET https://api.themoviedb.org/3/tv/{id}?api_key=K&append_to_response=credits` |
| Cast page | `GET https://api.themoviedb.org/3/person/{id}?api_key=K&append_to_response=images` |

Images are loaded from `https://image.tmdb.org/t/p/w500{path}` and `https://image.tmdb.org/t/p/w1280{path}`. Trailers are embedded from `https://www.youtube.com/embed/{youtube_trailer}`.

**Recommended:**
- Let users supply their own TMDB key, or proxy the calls.
- URL-encode the query.
- Match series on name plus year rather than an exact name compare.
- Make TMDB lookups opt-in, because they send viewing interests to a third party.
