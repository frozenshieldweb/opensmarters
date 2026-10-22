# Deploying opensmarters web

opensmarters web is the self-hosted web edition of opensmarters: one Docker container
that serves the player in the browser and talks to your IPTV provider for you
([WEB_PROMPT.md](WEB_PROMPT.md) has the design, [SECURITY.md §4.12](SECURITY.md#412-web-edition-cratesweb-opensmarters-web)
the threat model). This guide deploys it with [Dokploy](https://dokploy.com) on your
own server, step by step, then covers updates, backups, sizing and plain Docker.

## The private-instance rule

**Run it for yourself and your household, never as a public service.** There is no
public sign-up on purpose: the first visit creates the administrator with a one-time
setup code from the server's log, and the administrator invites the other people with
one-time links. Don't open sign-ups, don't share invitations publicly, and don't offer
the instance to strangers: an instance that relays IPTV for other people is exactly
what this project doesn't do. Each person adds their **own** provider account; the
provider's connection limit applies to every stream (each viewer uses one of the
account's connections).

## What you need

- A Linux server with [Dokploy installed](https://docs.dokploy.com/docs/core/installation)
  (it brings Docker and Traefik). 2 CPU cores and 2 GB of RAM are plenty for a household
  without video transcoding (see [Sizing](#sizing)).
- A domain or subdomain (for example `tv.example.com`) with a DNS `A`/`AAAA` record
  pointing at the server. HTTPS is **required**: sessions use `Secure` cookies, which
  browsers only keep over HTTPS (or on `localhost`).

## 1. Generate the secret key

The secret key encrypts your providers' passwords and playlist addresses at rest and
signs sessions and image addresses. It is 32 random bytes in base64:

```bash
openssl rand -base64 32
# or, once an image exists: docker run --rm opensmarters-web --generate-secret-key
```

Keep it in your password manager. If it is lost or changed, everyone is signed out and
every provider has to be entered again (nothing else is lost).

## 2. Create the application in Dokploy

1. In Dokploy, open (or create) a **Project**, then **Create Service → Application**.
   Name it `opensmarters-web`.
2. **General → Provider:** choose **GitHub** and the repository
   `frozenshieldweb/opensmarters` (after connecting Dokploy's GitHub app under
   *Settings → Git*), or choose **Git** and enter
   `https://github.com/frozenshieldweb/opensmarters.git`. Branch: `main`. Save.
3. **General → Build Type:** choose **Dockerfile**.
   - Docker File: `deploy/web/Dockerfile`
   - Docker Context Path: `.` (the repository root)
   - Docker Build Stage: leave empty

   Save.

## 3. Environment variables

In the **Environment** tab, add one `NAME=value` per line:

```bash
OPENSMARTERS_SECRET_KEY=<the key from step 1>
OPENSMARTERS_PUBLIC_URL=https://tv.example.com
OPENSMARTERS_TRUSTED_PROXY=private
```

Every variable the server reads:

| Variable | Default | What it does |
|---|---|---|
| `OPENSMARTERS_SECRET_KEY` | — **required** | 32 random bytes in base64 (step 1). The container refuses to start without it (only `--demo` makes up a temporary one). |
| `OPENSMARTERS_SECRET_KEY_FILE` | — | Read the key from a file instead (Docker secrets: `/run/secrets/…`). |
| `OPENSMARTERS_DATA_DIR` | `/data` in the image | The database, the image cache and the temporary stream folders. Mount a volume here. |
| `OPENSMARTERS_LISTEN` | `0.0.0.0:8080` | Address and port inside the container (plain HTTP; Traefik does TLS). |
| `OPENSMARTERS_PUBLIC_URL` | — | Your instance's address, used in invitation links. Without it the links use the address the administrator's browser used. |
| `OPENSMARTERS_TRUSTED_PROXY` | off | Whose `X-Forwarded-For` / `-Proto` / `-Host` to believe, for the sign-in rate limit and links: `private` (any proxy on a private network, such as Traefik on Dokploy's Docker network), a comma-separated list of IP addresses or CIDR ranges, or `any`. Leave it off when the container is reachable directly. |
| `OPENSMARTERS_MAX_STREAMS` | `8` | Streams the whole instance plays at once. |
| `OPENSMARTERS_STREAMS_PER_USER` | `1` | Streams one person plays at once. Starting another channel in the same browser replaces the previous stream; a second device gets "You're already watching on another device". |
| `OPENSMARTERS_TRANSCODE_VIDEO` | `never` | `never`: video is copied as is (H.264 everywhere; HEVC where the browser supports it). `hevc`: HEVC is converted to H.264 for browsers that can't play it. `always`: any video a browser can't play (HEVC, MPEG-2, MPEG-4 Part 2…) is converted. Converting video costs about one CPU core per stream. Audio is always converted to AAC when needed (cheap). |
| `OPENSMARTERS_ALLOW_PRIVATE_PROVIDERS` | `0` | `1` allows providers on private networks (your LAN, `10.x`, `192.168.x`, `fd00::/8`, CGNAT). Loopback, link-local (cloud metadata), the Docker host's gateway and reserved ranges stay refused. |
| `OPENSMARTERS_SETUP_CODE` | random | A setup code of your choice (8–64 visible characters). Without it, a random one-time code is printed in the log at start while nobody has signed up yet. |
| `OPENSMARTERS_LOG` | `info` | `error`, `warn`, `info`, `debug` or `trace`. Logs never contain passwords or provider addresses with credentials (they are redacted). |

## 4. A volume for `/data`

In **Advanced → Volumes** (called *Mounts* in some versions), add a **Volume Mount**:
name `opensmarters-data`, mount path `/data`. Without it, everything (people,
providers, favourites) is lost at each deployment.

## 5. Domain and HTTPS

In the **Domains** tab, add your domain:

- Host: `tv.example.com`
- Path: `/`
- Container Port: `8080`
- HTTPS: on, Certificate: **Let's Encrypt**

Traefik terminates HTTPS and forwards plain HTTP to the container on Dokploy's network.

## 6. Deploy

Press **Deploy** and follow the build in **Deployments**. The first build takes a while
(the server is compiled in release mode; 10 to 30 minutes on a small server); later
builds reuse Docker's cache. The image has a health check (`opensmarters-web
--healthcheck`); Dokploy shows the container as healthy once it answers.

## 7. Create the administrator

Open **Logs** (the container's log) and find the line:

```text
WARN no administrator yet: open the web page and enter this one-time setup code: ABCD-EFGH-JKLM
```

Open `https://tv.example.com`, choose **First run**, enter the setup code, your name and
a password (at least 10 characters). The code works once; a restart before setup prints
a new one. Then add your provider in **Settings → Providers** (Xtream Codes: server,
user name, password; or an M3U playlist address with an optional EPG address).

## 8. Invite people

**Settings → Users → Invite** creates a link valid for 72 hours and one sign-up. Send it
privately (the code is after the `#`, so it never reaches a server log). The person
chooses their name and password and then adds their own provider. In the same screen you
can disable someone (their sessions and streams end at once), enable them again or
remove them (with their providers, favourites and progress).

## Updating

Dokploy → the application → **Deploy** again (or turn on *Autodeploy* to deploy on every
push to `main`). The volume keeps all data. The database is migrated forward
automatically at start; an older version refuses a database written by a newer one, so
**back up before updating** if you might want to go back.

## Backups

`/data` contains:

| Path | What | Back up? |
|---|---|---|
| `opensmarters-web.db` (+ `-wal`, `-shm`) | People, sessions, invitations, providers (secrets encrypted), favourites, progress, settings | **Yes** |
| `cache/images/` | Downloaded artwork (rebuilt on demand) | No |
| `streams/` | Temporary HLS files of streams being watched (emptied at start) | No |
| `demo-media/` | Only with `--demo` | No |

Keep the **secret key** with the backup but not in it (the providers' secrets can't be
read without it). A consistent copy: stop the container, copy
`opensmarters-web.db*`, start it again; or, while it runs,
`sqlite3 /data/opensmarters-web.db ".backup '/data/backup.db'"` from a container that
has `sqlite3`. Dokploy's **Volume Backups** (to S3-compatible storage) can schedule this.
To restore, stop the container, put the files back in the volume, start it with the
same secret key.

## Sizing

Every stream passes through the server: it downloads from the provider and uploads to
the viewer.

| Resource | Per stream | Notes |
|---|---|---|
| Bandwidth | 3–8 Mbit/s in **and** out for HD (15–25 for 4K) | The provider-to-server and server-to-viewer directions both count |
| CPU (video copied) | a few % of one core | Audio is converted to AAC when it isn't AAC |
| CPU (`OPENSMARTERS_TRANSCODE_VIDEO`) | about 1 core for 1080p | `veryfast` H.264; size `OPENSMARTERS_MAX_STREAMS` to your cores |
| RAM | about 50 MB | Plus about 100 MB for the server and caches |
| Disk (`/data/streams`) | live: a few MB; films: what was watched plus about 2 minutes ahead | A whole HD film is 2–4 GB; the folder is deleted when playback stops |

Each viewer also uses one of the provider account's connections; providers keep counting
a closed connection for one to three minutes.

## Plain Docker or Docker Compose

```bash
git clone https://github.com/frozenshieldweb/opensmarters && cd opensmarters
docker build -f deploy/web/Dockerfile -t opensmarters-web .
docker run -d --name opensmarters-web --restart unless-stopped \
  -p 8080:8080 -v opensmarters-data:/data \
  -e OPENSMARTERS_SECRET_KEY="$(openssl rand -base64 32)" \
  --read-only --tmpfs /tmp --cap-drop ALL --security-opt no-new-privileges:true \
  opensmarters-web
docker logs opensmarters-web        # the setup code
```

(Save the key you generated: the providers can't be read with another one.) Or with
Compose: `deploy/web/compose.yaml` (`docker compose -f deploy/web/compose.yaml up -d
--build`, with `OPENSMARTERS_SECRET_KEY` exported); for Dokploy's *Compose* service type,
remove its `ports:` and add a domain. Put any reverse proxy in front for HTTPS
(Caddy, nginx, Traefik) and set `OPENSMARTERS_TRUSTED_PROXY` to its address.

## Trying the demo

`--demo` starts the built-in synthetic provider inside the container and a demo person
(sign in as **`demo`** / **`demo-password`**); no real provider is involved:

```bash
docker run --rm -p 8080:8080 -e OPENSMARTERS_SECRET_KEY="$(openssl rand -base64 32)" \
  opensmarters-web --demo
```

Open `http://localhost:8080` (localhost is allowed without HTTPS). The demo's test videos
are generated with ffmpeg on the first start (about 20 seconds). Never use `--demo` on
an instance people rely on.

## Troubleshooting

- **Signing in works but the next page signs you out:** the site isn't served over HTTPS
  (browsers drop `Secure` cookies on plain HTTP except on `localhost`).
- **"That address is on a private network…":** the provider is on your LAN; set
  `OPENSMARTERS_ALLOW_PRIVATE_PROVIDERS=1`.
- **"Your provider refused the stream…":** usually all of the account's connections are
  in use (another app, or a stream that ended less than a few minutes ago).
- **"This video is in HEVC…":** the browser can't play HEVC; set
  `OPENSMARTERS_TRANSCODE_VIDEO=hevc` if the server has the CPU for it.
- **Playback stops after a long pause:** a stream nobody fetches for 30 seconds is
  stopped (to free the provider connection); the player starts it again when you
  press play.
- **The page says the interface isn't built:** the image was built from a checkout
  without `web/`; rebuild from a revision that has it.
