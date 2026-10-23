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

- **A Linux server** (a VPS or a dedicated server; Ubuntu or Debian is what Dokploy's
  installer expects), reachable from the internet on ports **80** and **443**. **2 GB of
  RAM** is enough: Dokploy compiles the server when it deploys, which fits in 2 GB as long
  as at most two compilers run at once (see [step 0](#step-0-prepare-the-server));
  running it afterwards needs much less (see [Sizing](#sizing)).
- **A domain or subdomain** for the player, for example `tv.example.com`, with a DNS `A`
  record (and `AAAA` for IPv6) pointing at the server. HTTPS is **required**: sessions use
  `Secure` cookies, which browsers only keep over HTTPS (or on `localhost`).
- About **30 minutes**, most of it waiting for the first build.

## Deploy with Dokploy, step by step

### Step 0: prepare the server

1. Install Dokploy (it brings Docker, Docker Swarm and Traefik) by following
   [Dokploy's installation guide](https://docs.dokploy.com/docs/core/installation). At the
   time of writing that is one command, run as root:

   ```bash
   curl -sSL https://dokploy.com/install.sh | sh
   ```

   Then open `http://<server-ip>:3000`, create Dokploy's administrator account, and
   (recommended) give Dokploy's own panel a domain with HTTPS in its settings.
2. Make sure ports **80** and **443** are open in the server's firewall (Let's Encrypt
   uses port 80 to issue the certificate).
3. **Memory for the build.** Each compiler that runs at once can take up to about 1 GB,
   and the build runs one per CPU core. Measured: a clean build on 2 cores within 2 GB of
   memory succeeds; the same build with many cores at once is stopped by the kernel.
   - **2 cores, 2 GB or more:** nothing to do.
   - **More cores than gigabytes of RAM** (say 8 cores, 4 GB): set the build argument
     `CARGO_BUILD_JOBS=2` in [step 4](#step-4-choose-how-it-is-built), or add swap.
   - **Less than 2 GB:** add swap before the first deployment:

   ```bash
   sudo fallocate -l 4G /swapfile && sudo chmod 600 /swapfile
   sudo mkswap /swapfile && sudo swapon /swapfile
   echo '/swapfile none swap sw 0 0' | sudo tee -a /etc/fstab
   ```

4. In your DNS, create the `A` record for `tv.example.com` (your domain) pointing at the
   server's public IP. It can take a few minutes to take effect.

**Check:** `ping tv.example.com` answers from the server's IP, and Dokploy's panel opens.

### Step 1: generate the secret key

The secret key encrypts your providers' passwords and playlist addresses at rest, and
signs sessions and image addresses. It is 32 random bytes in base64. On any computer:

```bash
openssl rand -base64 32
```

Copy the result into your password manager now. **If it is lost or changed, everyone is
signed out and every provider has to be entered again** (nothing else is lost).

### Step 2: create the application

1. In Dokploy, open **Projects** → **Create Project**, name it `opensmarters`.
2. In the project: **Create Service** → **Application**, name it `opensmarters-web`.

**Check:** the application's page opens with its tabs (General, Environment, Domains,
Deployments, Logs, Advanced…).

### Step 3: point it at the source code

In the **General** tab, under the source (**Provider**):

- **GitHub** (recommended, allows automatic deployments): connect Dokploy's GitHub app
  once under Dokploy's *Settings → Git* if you haven't, then choose the repository
  `frozenshieldweb/opensmarters` and the branch **`main`**;
- or **Git**: repository URL `https://github.com/frozenshieldweb/opensmarters.git`,
  branch **`main`**.

Save.

### Step 4: choose how it is built

Still in **General**, set **Build Type** to **Dockerfile** and fill in:

| Field | Value |
|---|---|
| **Dockerfile Path** | `deploy/web/Dockerfile` |
| **Docker Context Path** | `.` |
| **Docker Build Stage** | *(leave empty)* |

Save. (The Dockerfile builds the browser interface, then the server, then a small
runtime image; [How the image is built](#how-the-image-is-built) has the details.)

**Only if the server has more CPU cores than gigabytes of RAM:** open the **Environment**
tab → **Build Time Arguments** and add `CARGO_BUILD_JOBS=2` (it limits how many compilers
run at once during the build; it changes nothing at run time).

### Step 5: environment variables

Open the **Environment** tab and paste, with your own values:

```bash
OPENSMARTERS_SECRET_KEY=<the key from step 1>
OPENSMARTERS_PUBLIC_URL=https://tv.example.com
OPENSMARTERS_TRUSTED_PROXY=private
```

**On a server with CPU to spare** (a dedicated server), also add:

```bash
OPENSMARTERS_TRANSCODE_VIDEO=hevc
```

Some providers send channels in HEVC, which Firefox and some Chrome setups can't play;
with this setting the server converts those streams to H.264 (about one CPU core per
HEVC stream being watched), so every channel plays in every browser. Other streams are
still copied as they are.

Save. The first three are all you need. `OPENSMARTERS_TRUSTED_PROXY=private` lets the
server see the visitor's real address behind Traefik (for the sign-in rate limit); the
public URL is used in invitation links. Every other setting is optional and listed in
[Environment variables](#environment-variables). Don't put the key in **Build Time
Arguments**: it is only needed when the container runs.

### Step 6: storage for `/data`

Open **Advanced** → **Volumes** (Mounts) → add a **Volume Mount**:

| Field | Value |
|---|---|
| **Volume Name** | `opensmarters-data` |
| **Mount Path** | `/data` |

Save. **Without it, everything (people, providers, favourites, progress) is lost at the
next deployment.**

### Step 7: domain and HTTPS

Open **Domains** → **Add Domain**:

| Field | Value |
|---|---|
| **Host** | `tv.example.com` |
| **Path** | `/` |
| **Container Port** | `8080` |
| **HTTPS** | on |
| **Certificate** | `letsencrypt` |

Leave **Internal Path** and **Strip Path** as they are. Save. Traefik receives HTTPS on
port 443 and forwards plain HTTP to the container's port 8080 on Dokploy's internal
network; the container port is **not** published to the internet. (Don't add anything
under **Advanced → Ports**.)

### Step 8: deploy

Press **Deploy** (General tab) and follow the build in **Deployments** (click the running
deployment to see its log).

- The **first build usually takes 5 to 15 minutes**: the browser interface is built,
  then the server is compiled in release mode (about 2 minutes on two fast cores; slower
  virtual CPUs take longer), then ffmpeg is installed in the runtime image. Later builds
  reuse Docker's cache and are faster.
- The image has its own health check (`opensmarters-web --healthcheck`); the deployment
  finishes when the container is running.

**Check:** the deployment ends with *Done*, and **Logs** shows lines like
`listening addr=0.0.0.0:8080 ui_built=true`.

If the build stops with *Killed*, *signal: 9* or *exit code 137*, the server ran out of
memory: set `CARGO_BUILD_JOBS=2` (or `1`) as a build argument, or add swap
([step 0](#step-0-prepare-the-server)), and deploy again.

### Step 9: create the administrator

1. In **Logs**, find the line:

   ```text
   WARN no administrator yet: open the web page and enter this one-time setup code: ABCD-EFGH-JKLM
   ```

2. Open `https://tv.example.com`. While nobody has an account, the page shows the
   **set-up** screen by itself: enter the setup code, your name and a password (at least
   10 characters).
3. You're signed in. Add your provider when asked (or later in **Settings → Providers**):
   Xtream Codes (server, user name, password) or an M3U playlist address with an
   optional EPG address.

The code works once. Restarting the container before set-up prints a new one. (To choose
your own code instead, set `OPENSMARTERS_SETUP_CODE` in step 5.)

**Check:** the browser shows the padlock, Live TV lists your channels, and a channel
plays in the preview.

### Step 10: invite your household

**Settings → Users → Invite** creates a link that works once, for 72 hours. Send it
privately (the code is after the `#`, so it never reaches a server log). The person
chooses a name and password, then adds their **own** provider. The same screen can
disable someone (their sessions and streams end at once), enable them again, or remove
them with their providers, favourites and progress.

### Step 11 (optional): automatic deployments

To deploy every time `main` changes, turn on **Autodeploy** in the General tab (with the
GitHub provider; for the Git provider, add the webhook URL from **Deployments** to the
repository). Each deployment rebuilds the image, so you may prefer to deploy by hand when
you choose to update.

### A note on hardening

The image runs the server as an unprivileged user (uid 10001) with `tini` as PID 1, and
the server only listens inside Dokploy's network. Dokploy's **Application** type doesn't
offer a read-only root filesystem or dropping Linux capabilities. If you want those too,
deploy `deploy/web/compose.yaml` with Dokploy's **Docker Compose** type instead: it sets
`read_only`, a `/tmp` tmpfs, `cap_drop: ALL` and `no-new-privileges`. That route needs
its `ports:` removed, the service attached to Dokploy's `dokploy-network`, and Traefik
labels for the domain ([Dokploy: Docker Compose domains](https://docs.dokploy.com/docs/core/docker-compose)).

## Environment variables

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

## How the image is built

`deploy/web/Dockerfile` (build context: the repository root) has three stages:

1. **UI** (`node:22`): when `web/package.json` exists, `npm ci` (or `npm install` without a
   lock file) and `npm run build` in `web/`, keeping `web/dist`. Without `web/`, the stage
   produces an empty folder and the server shows a short built-in page saying the
   interface isn't built (the API works either way).
2. **Server** (`rust:1.99.1-slim-trixie`): rustup installs exactly the toolchain of
   `rust-toolchain.toml`, then `cargo build -p opensmarters-web --release --locked`
   embeds `web/dist` into the binary.
3. **Runtime** (`debian:trixie-slim`): `ffmpeg`, `ca-certificates` and `tini`, the user
   `opensmarters` (uid 10001), `/data` as a volume, port 8080, and a `HEALTHCHECK` that
   runs `opensmarters-web --healthcheck` (no curl in the image).

The root `.dockerignore` keeps `target/`, `dist/`, `vendor/`, `node_modules/` and test
data out of the build context.

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
--build`, with `OPENSMARTERS_SECRET_KEY` exported); for Dokploy's *Compose* service type
see [A note on hardening](#a-note-on-hardening). Put any reverse proxy in front for HTTPS
(Caddy, nginx, Traefik) and set `OPENSMARTERS_TRUSTED_PROXY` to its address.

## Trying the demo

`--demo` starts the built-in synthetic provider inside the container and a demo person
(sign in as **`demo`** / **`demo-password`**); no real provider is involved:

```bash
docker run --rm -p 8080:8080 -e OPENSMARTERS_SECRET_KEY="$(openssl rand -base64 32)" \
  opensmarters-web --demo
```

Open `http://localhost:8080` (localhost is allowed without HTTPS). The demo's test videos
are generated with ffmpeg before the server starts serving (a few seconds, on the first
start only); its live channels loop like real ones. Never use `--demo` on an instance
people rely on.

## Troubleshooting

- **The build stops with *Killed* / *exit code 137*:** not enough memory for the
  compilers running at once; set the build argument `CARGO_BUILD_JOBS=2` (or `1`), or add
  swap ([step 0](#step-0-prepare-the-server)).
- **The domain shows *Bad Gateway* or *404 page not found*:** the container isn't running
  yet (look at **Logs**), or the domain's **Container Port** isn't `8080`.
- **No HTTPS certificate (the browser warns):** the DNS record doesn't point at the server
  yet, or port 80 is closed (Let's Encrypt checks the domain on port 80). Fix it, then
  save the domain again.
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
