# Deploying `notes-server`

`notes-server` is a single binary. It keeps everything in one directory (`--data-dir`):
`notes.db` (SQLite: update log, snapshots, derived index, accounts, sessions) and
`attachments/` (content-addressed blobs). It speaks plain HTTP and expects **TLS to be
terminated in front of it** — a reverse proxy on a home server, or the platform's terminator on
fly.io.

Two deployment shapes are covered here. Both use the [`Dockerfile`](../Dockerfile) at the repo
root, which builds the web client (`ui/dist`), `notes-server`, and the `notes` CLI, and ships
them on `debian:bookworm-slim` as uid `10001`.

## Flags and environment variables

Every `notes-server` flag has an environment variable, so you can configure the container
either way. From `crates/server/src/main.rs`:

| Flag | Env var | Default |
|---|---|---|
| `--bind <ADDR>` | `NOTES_BIND` | `127.0.0.1:8080` |
| `--data-dir <DIR>` | `NOTES_DATA_DIR` | `./data` |
| `--web-dir <DIR>` | `NOTES_WEB_DIR` | unset (API + sync only, no web client) |
| `--no-auth` | `NOTES_NO_AUTH` | off |
| `--allow-registration` | `NOTES_ALLOW_REGISTRATION` | off |
| `--secure-cookies` | `NOTES_SECURE_COOKIES` | off |
| `--snapshot-every-updates <N>` | `NOTES_SNAPSHOT_EVERY_UPDATES` | `500` |
| `--snapshot-every-minutes <N>` | `NOTES_SNAPSHOT_EVERY_MINUTES` | `10` |
| `--retain-days <N>` | `NOTES_RETAIN_DAYS` | `90` |
| `--attachment-grace-days <N>` | `NOTES_ATTACHMENT_GRACE_DAYS` | `30` |

The four boolean flags accept **only `true` or `false`** when set through the environment —
`NOTES_NO_AUTH=1` is not a synonym for `true`, it is a startup error:

```
error: invalid value '1' for '--no-auth'
  [possible values: true, false]
```

`NOTES_SECURE_COOKIES=false` is valid and equivalent to leaving the variable unset. Use the
spelled-out words in Compose files, `fly.toml`, and systemd units.

Log verbosity is `RUST_LOG` (`tracing_subscriber::EnvFilter`), defaulting to
`info,tower_http=debug`.

The image's default command is:

```
notes-server --bind 0.0.0.0:8080 --data-dir /data --web-dir /app/web
```

Anything you append to `docker run … notes <args>` replaces that whole list, so repeat the
three flags if you add a fourth. Adding an environment variable does not have this problem.

Liveness endpoint: `GET /healthz` → `ok`, unauthenticated.

---

## (a) Docker on a home server, behind Caddy

### Run the container

```sh
docker volume create notes_data

docker run -d --name notes \
  --restart unless-stopped \
  -p 127.0.0.1:8080:8080 \
  -v notes_data:/data \
  -e NOTES_SECURE_COOKIES=true \
  -e RUST_LOG=info \
  ghcr.io/you/notes:latest      # or a locally built `notes` tag
```

`-p 127.0.0.1:8080:8080` publishes only on loopback: the proxy reaches it, the LAN does not.

Set `NOTES_SECURE_COOKIES` (the `--secure-cookies` flag) whenever users reach the server over
HTTPS. It marks the browser session cookie `Secure`; without it a proxy-terminated HTTPS site
still works, but the cookie is also allowed to travel over plain HTTP. Do not set it if you are
genuinely serving over `http://` — the browser will refuse to store the cookie and login will
appear to silently fail.

**Volume ownership.** The container runs as uid `10001`. A *named* volume (as above) inherits
`/data`'s ownership from the image, so it just works. A *bind mount* does not — the host
directory keeps its own ownership and the server cannot create `notes.db`:

```sh
mkdir -p /srv/notes/data && chown -R 10001:10001 /srv/notes/data
docker run … -v /srv/notes/data:/data …
```

### Caddyfile

```caddyfile
notes.example.org {
	# Caddy obtains and renews a Let's Encrypt certificate automatically.
	reverse_proxy 127.0.0.1:8080
}
```

That is the whole configuration. Two things worth knowing:

- **WebSockets need no special handling.** The sync relay lives at `/ws`, one long-lived socket
  per client. Caddy's `reverse_proxy` passes `Upgrade`/`Connection` through and streams
  bidirectionally by default — the `@websockets` matcher blocks you may remember are Caddy v1
  and are unnecessary in v2.
- **Attachment uploads are large.** Caddy does not impose a request body limit unless you add
  `request_body { max_size … }`, so leave it out (or set it above the server's own attachment
  cap).

If you terminate TLS with nginx instead, you *do* need the explicit upgrade headers
(`proxy_set_header Upgrade $http_upgrade; proxy_set_header Connection "upgrade";
proxy_http_version 1.1;`) plus `proxy_read_timeout 3600s;` so idle relay sockets are not
dropped, and `client_max_body_size 0;`.

### First login

```sh
notes login --server https://notes.example.org --email you@example.org --register
notes sync  --vault ~/vault --server https://notes.example.org
```

---

## (b) fly.io

[`fly.toml`](../fly.toml) at the repo root is a ready template; replace the `app` name and
`primary_region` placeholders first.

```sh
# 1. Claim the app name. --no-deploy stops fly from building before the volume exists;
#    answer "no" when it offers to overwrite fly.toml.
fly launch --no-deploy

# 2. Create the volume the [[mounts]] block refers to, in the app's primary region.
#    3 GB is a comfortable start; `fly volumes extend` grows it later.
fly volumes create notes_data --size 3

# 3. Build the Dockerfile and release it.
fly deploy

# 4. Watch it come up; the [checks] block polls /healthz.
fly status
fly logs
```

Then register the first account against the deployed URL:

```sh
notes login --server https://notes-yourname.fly.dev --email you@example.org --register
notes sync  --vault ~/vault --server https://notes-yourname.fly.dev
```

`fly.toml` sets `NOTES_SECURE_COOKIES = "true"` because `force_https = true` guarantees HTTPS,
and `auto_stop_machines = "off"` with `min_machines_running = 1` because the relay must stay
reachable for clients holding sync sockets — a suspended machine looks like an outage and
pushes every client into reconnect backoff.

**One machine only.** `notes.db` is a single SQLite file on a single volume; two machines cannot
share it and Fly will not attach one volume to two machines. Scale up (`fly scale vm`), never
out (`fly scale count`).

**Volume ownership on Fly.** Fly attaches volumes as an empty root-owned filesystem, so the
non-root container cannot write to `/data` on the very first boot. If the first deploy crashes
with a permission error creating `notes.db`, fix it once:

```sh
fly ssh console --user root -C "chown -R 10001:10001 /data"
fly apps restart notes-yourname
```

---

## (c) Backups

Everything that matters is `notes.db` plus `attachments/`. Do **not** copy `notes.db` with `cp`
while the server is running — it is in WAL mode and you would capture a torn database. Use
SQLite's own online backup, which is safe against a live writer:

```sh
# Inside the container (sqlite3 is not in the runtime image — install it, or run from the host
# against the volume's path).
sqlite3 /data/notes.db ".backup '/data/backup/notes-$(date +%F).db'"

# Attachments are immutable content-addressed blobs, so a plain incremental copy is fine.
rsync -a /data/attachments/ /backup/notes/attachments/
```

From the host, with the container running and the data in a named volume:

```sh
mkdir -p backup

# notes.db, via SQLite's online backup (alpine's sqlite package, no third-party image).
docker run --rm -v notes_data:/data -v "$PWD/backup:/backup" alpine:3 \
  sh -c "apk add --no-cache sqlite >/dev/null && \
         sqlite3 /data/notes.db \".backup '/backup/notes-\$(date +%F).db'\""

# attachments/ — immutable content-addressed blobs, so a plain copy is enough.
docker run --rm -v notes_data:/data -v "$PWD/backup:/backup" alpine:3 \
  cp -a /data/attachments /backup/attachments
```

Both commands are safe to run against a live server: `.backup` takes a consistent snapshot
through SQLite itself, and blobs are never rewritten in place.

On fly.io, `fly volumes snapshots list <volume-id>` shows the automatic daily snapshots
(retained 5 days by default); `fly ssh console` + the `.backup` command above gets you an
off-platform copy, which the snapshots are not a substitute for.

Restoring is the reverse: stop the server, put `notes.db` and `attachments/` back in the data
directory, start it. Sync clients reconcile from their own journals on reconnect, so a restore
to a slightly older state does not lose work that is still on a client.

Note that `--retain-days` (default 90) prunes raw update history that snapshots have made
redundant; versions themselves are kept forever. Backups are your only protection against
losing fine-grained history older than that window.

---

## (d) Security notes

**Never run `--no-auth` on a network.** It is a development switch: it sets
`AuthMode::Disabled`, and every request — REST, relay frames, attachment uploads — is then
treated as a local owner with no token at all. Anyone who can reach the port owns every vault.
The server logs a warning at startup when it is on. The same applies to `NOTES_NO_AUTH`, which
is the same switch by another name; keep it out of Compose files and `fly secrets`.

**The first registered account is the admin.** Registration is allowed when *any* of these hold
(`crates/server/src/auth.rs`): the user table is empty, `--allow-registration` is set, or the
request carries the session of an existing admin. So on a fresh server the very first
`POST /api/v1/auth/register` — i.e. `notes login --register` — succeeds without credentials and
creates an admin; every later attempt is `403` unless one of the other two conditions applies.

Deploy and register immediately. Between `fly deploy` and your first `notes login --register`,
whoever reaches the URL first becomes the admin.

**`--allow-registration` opens self-service signup to the whole internet.** Leave it off for a
personal or small-team server and have the admin create accounts (an admin's
`POST /api/v1/auth/register` creates the user without logging the admin out of their own
session). Turn it on only behind something else that restricts who can reach the server. There
is no invite or domain allowlist; the only validation on a new account is that the email
contains `@` and the password is at least 8 characters.

**Other things worth doing:**

- Set `--secure-cookies` on every HTTPS deployment (see above).
- Sessions are opaque bearer tokens, hashed at rest. `notes logout --server <url>` forgets the
  local copy.
- There is no end-to-end encryption by design (SPEC §15) — the server reads note content in
  order to index, search, and share it. Encrypt the disk or volume if that matters.
- Vault roles (owner / editor / viewer) are enforced on both REST and every relay frame, but a
  vault that nobody owns yet is claimed by the first user who syncs it. On a shared server,
  create and claim your vaults before handing out accounts.
