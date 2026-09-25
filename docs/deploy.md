# Deploying `lemmate-server`

`lemmate-server` is a single binary. It keeps everything in one directory (`--data-dir`):
`lemmate.db` (SQLite: update log, snapshots, derived index, accounts, sessions) and
`attachments/` (content-addressed blobs). It speaks plain HTTP and expects **TLS to be
terminated in front of it** — a reverse proxy, Caddy in the recipe below.

The recipe is the same on a machine at home and on a rented one — see (b). It uses the
[`Dockerfile`](../Dockerfile) at the repo root, which builds the web client (`ui/dist`),
`lemmate-server`, and the `lemmate` CLI, and ships them on `debian:bookworm-slim` as uid `10001`.

## Flags and environment variables

Every `lemmate-server` flag has an environment variable, so you can configure the container
either way. From `crates/server/src/main.rs`:

| Flag | Env var | Default |
|---|---|---|
| `--bind <ADDR>` | `LEMMATE_BIND` | `127.0.0.1:8080` |
| `--data-dir <DIR>` | `LEMMATE_DATA_DIR` | `./data` |
| `--web-dir <DIR>` | `LEMMATE_WEB_DIR` | unset (API + sync only, no web client) |
| `--no-auth` | `LEMMATE_NO_AUTH` | off |
| `--allow-registration` | `LEMMATE_ALLOW_REGISTRATION` | off |
| `--pandoc PATH` | `LEMMATE_PANDOC` | pandoc binary for exports (default: on `PATH`; exports answer 501 without it) |
| `--quarto PATH` | `LEMMATE_QUARTO` | quarto binary for *Render with Quarto* (default: on `PATH`; renders answer 501 without it) |
| `--disable-quarto` | `LEMMATE_DISABLE_QUARTO` | off — set it to refuse renders even with quarto installed |
| `--secure-cookies` | `LEMMATE_SECURE_COOKIES` | off |
| `--snapshot-every-updates <N>` | `LEMMATE_SNAPSHOT_EVERY_UPDATES` | `500` |
| `--snapshot-every-minutes <N>` | `LEMMATE_SNAPSHOT_EVERY_MINUTES` | `10` |
| `--retain-days <N>` | `LEMMATE_RETAIN_DAYS` | `90` |
| `--attachment-grace-days <N>` | `LEMMATE_ATTACHMENT_GRACE_DAYS` | `30` |
| `--public-url <URL>` | `LEMMATE_PUBLIC_URL` | unset — the address people reach the server at; needed for OIDC |
| `--oidc-issuer <URL>` | `LEMMATE_OIDC_ISSUER` | unset — setting it turns OIDC sign-in on (§d) |
| `--oidc-client-id <ID>` | `LEMMATE_OIDC_CLIENT_ID` | unset |
| `--oidc-client-secret <S>` | `LEMMATE_OIDC_CLIENT_SECRET` | unset (a public client) |
| `--oidc-client-secret-file <F>` | `LEMMATE_OIDC_CLIENT_SECRET_FILE` | unset — the secret from a file, for Docker/systemd secrets |
| `--oidc-name <TEXT>` | `LEMMATE_OIDC_NAME` | `single sign-on` — the sign-in button says *Sign in with …* |
| `--oidc-scopes <S>` | `LEMMATE_OIDC_SCOPES` | `openid email profile` |
| `--disable-password-login` | `LEMMATE_DISABLE_PASSWORD_LOGIN` | off — set it to sign in through OIDC only |

The six boolean flags accept **`true`/`false` (also `1`/`0`, `yes`/`no`, `on`/`off`)** when set through the environment —

```
error: invalid value '1' for '--no-auth'
  [possible values: true, false]
```

`LEMMATE_SECURE_COOKIES=false` is valid and equivalent to leaving the variable unset. Use the
spelled-out words in Compose files and systemd units.

Log verbosity is `RUST_LOG` (`tracing_subscriber::EnvFilter`), defaulting to
`info,tower_http=debug`.

The image's default command is:

```
lemmate-server --bind 0.0.0.0:8080 --data-dir /data --web-dir /app/web
```

Anything you append to `docker run … lemmate <args>` replaces that whole list, so repeat the
three flags if you add a fourth. Adding an environment variable does not have this problem.

Liveness endpoint: `GET /healthz` → `ok`, unauthenticated.

---

## (a) Docker behind Caddy

### Run the container

```sh
docker build -t lemmate .          # from the repository root
docker volume create lemmate_data

docker run -d --name lemmate \
  --restart unless-stopped \
  -p 127.0.0.1:8080:8080 \
  -v lemmate_data:/data \
  -e LEMMATE_SECURE_COOKIES=true \
  -e RUST_LOG=info \
  lemmate
```

`-p 127.0.0.1:8080:8080` publishes only on loopback: the proxy reaches it, the LAN does not.

Set `LEMMATE_SECURE_COOKIES` (the `--secure-cookies` flag) whenever users reach the server over
HTTPS. It marks the browser session cookie `Secure`; without it a proxy-terminated HTTPS site
still works, but the cookie is also allowed to travel over plain HTTP. Do not set it if you are
genuinely serving over `http://` — the browser will refuse to store the cookie and login will
appear to silently fail.

**What is in the image.** The server, the `lemmate` CLI, the web client, and
[Quarto](https://quarto.org) — for *Render with Quarto* (HTML, PDF through Typst, DOCX, slides)
and for its bundled pandoc, which is linked onto `PATH` so the plain exports work too. Quarto
is most of the image's size (about 450 MB unpacked); `docker build --build-arg WITH_QUARTO=0 .`
leaves it out, and rendering and export then answer 501. `--build-arg QUARTO_VERSION=…` picks
another release. PDF export through pandoc still needs a LaTeX engine, which is not included;
Quarto's PDF does not.

**Volume ownership.** The container runs as uid `10001`. A *named* volume (as above) inherits
`/data`'s ownership from the image, so it just works. A *bind mount* does not — the host
directory keeps its own ownership and the server cannot create `lemmate.db`:

```sh
mkdir -p /srv/lemmate/data && chown -R 10001:10001 /srv/lemmate/data
docker run … -v /srv/lemmate/data:/data …
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
lemmate login --server https://notes.example.org --email you@example.org --register
lemmate sync  --vault ~/vault --server https://notes.example.org
```

---

## (b) Without a machine at home

Rent a small Linux server with a persistent disk and run (a) on it unchanged:
Docker, the container, Caddy in front, a DNS name pointing at it. Nothing in Lemmate is tied to
a provider, and there is no platform-specific configuration to keep up to date. A few things to
look for when choosing one:

- **Size.** The server itself is small — one process, one SQLite file. A single vCPU and 1 GB of
  RAM is plenty for a handful of users; Quarto renders are the heaviest thing it does. The disk
  holds the history and every attachment, so size it for those.
- **It must stay up.** Clients hold a sync socket open. A platform that suspends idle
  containers or scales to zero looks like an outage to them and pushes every client into
  reconnect backoff — pick a plain virtual machine, or turn that behaviour off.
- **One machine only.** `lemmate.db` is a single SQLite file on a single disk; two instances
  cannot share it. Scale up, never out.
- **Ports 80 and 443 reachable**, so Caddy can obtain its certificate.

Build the image on your own machine rather than on the server: compiling the Rust workspace
wants several GB of RAM, which a small VM does not have. Then copy it across:

```sh
docker build -t lemmate .
docker save lemmate | gzip | ssh you@server 'gunzip | docker load'
```

Register the first account as soon as the server answers — (e) explains why — and set up
backups as in (c): a rented disk is no safer than one at home.

---

## (c) Backups

Everything that matters is `lemmate.db` plus `attachments/`. Do **not** copy `lemmate.db` with `cp`
while the server is running — it is in WAL mode and you would capture a torn database. Use
SQLite's own online backup, which is safe against a live writer:

```sh
# Inside the container (sqlite3 is not in the runtime image — install it, or run from the host
# against the volume's path).
sqlite3 /data/lemmate.db ".backup '/data/backup/lemmate-$(date +%F).db'"

# Attachments are immutable content-addressed blobs, so a plain incremental copy is fine.
rsync -a /data/attachments/ /backup/lemmate/attachments/
```

From the host, with the container running and the data in a named volume:

```sh
mkdir -p backup

# lemmate.db, via SQLite's online backup (alpine's sqlite package, no third-party image).
docker run --rm -v lemmate_data:/data -v "$PWD/backup:/backup" alpine:3 \
  sh -c "apk add --no-cache sqlite >/dev/null && \
         sqlite3 /data/lemmate.db \".backup '/backup/lemmate-\$(date +%F).db'\""

# attachments/ — immutable content-addressed blobs, so a plain copy is enough.
docker run --rm -v lemmate_data:/data -v "$PWD/backup:/backup" alpine:3 \
  cp -a /data/attachments /backup/attachments
```

Both commands are safe to run against a live server: `.backup` takes a consistent snapshot
through SQLite itself, and blobs are never rewritten in place.

If the provider offers disk snapshots, they are a useful extra but not a backup: they live on
the same platform as the server. Keep a `.backup` copy somewhere else.

Restoring is the reverse: stop the server, put `lemmate.db` and `attachments/` back in the data
directory, start it. Sync clients reconcile from their own journals on reconnect, so a restore
to a slightly older state does not lose work that is still on a client.

Note that `--retain-days` (default 90) prunes raw update history that snapshots have made
redundant; versions themselves are kept forever. Backups are your only protection against
losing fine-grained history older than that window.

---

## (d) Letting people in, and changing passwords

Invites and admin password resets exist because a self-hosted server has no mail: there is no
confirmation email and no reset-by-email link, and none is planned (SPEC §15). If you already run
an identity provider, let it do this instead (below).

### Signing in through an identity provider (OIDC)

Register Lemmate with the provider as a confidential client using the authorization-code flow,
with the redirect URI `https://notes.example.org/api/v1/auth/oidc/callback`, the scopes `openid
email profile`, and client authentication `client_secret_basic`. Then:

```yaml
    environment:
      LEMMATE_PUBLIC_URL: https://notes.example.org
      LEMMATE_OIDC_ISSUER: https://auth.example.org        # exactly as its discovery document says
      LEMMATE_OIDC_CLIENT_ID: lemmate
      LEMMATE_OIDC_CLIENT_SECRET_FILE: /run/secrets/lemmate_oidc
      LEMMATE_OIDC_NAME: Authelia
      # LEMMATE_DISABLE_PASSWORD_LOGIN: "true"             # once everyone has signed in through it
```

For Authelia, the client entry is roughly:

```yaml
identity_providers:
  oidc:
    clients:
      - client_id: lemmate
        client_name: Lemmate
        client_secret: '$pbkdf2-sha512$…'                  # the hash of the secret Lemmate is given
        redirect_uris: [https://notes.example.org/api/v1/auth/oidc/callback]
        scopes: [openid, email, profile]
        authorization_policy: two_factor
        token_endpoint_auth_method: client_secret_basic
```

The server fetches the provider's discovery document on the first sign-in, not at startup, so it
starts even when the provider is not up yet. It needs to reach the provider over HTTPS itself —
the ID token is taken from the token endpoint over that connection, which is what lets the server
skip checking its signature (OIDC Core §3.1.3.7); issuer, audience, expiry and nonce are checked.
Plain `http://` issuers are refused except on a loopback address.

Who gets an account: an identity whose **verified** email matches an existing account is tied to
it (so a password account moves over on its first OIDC sign-in); a new identity gets an account
only on an empty server (the admin), with `--allow-registration`, or through an invite link —
the invite page offers *Accept the invite with …*. Authelia keeps the email out of the ID token by
default; the server then reads it from the userinfo endpoint.

`--disable-password-login` removes email + password sign-in, password registration and
password changes; the server refuses to start with it and no OIDC issuer. The CLI, MCP and the
desktop app then sign in with an **access token** made in the web client (*Account → Access
tokens*): `lemmate login --server … --token lmt_…`, or the token field in the desktop app's
setup and *Connect a server…* dialogs.

### Invites

An admin mints a **single-use registration link**. It carries a random token — the server keeps
only its BLAKE3 hash — and creates exactly one non-admin account before it stops working:

```sh
lemmate invite --server https://notes.example.org
# https://notes.example.org/#/invite/<token>
# single use; send it however you like. Revoke with `lemmate invite --revoke <id>`

lemmate invite --server … --expires-days 7   # optional deadline; still single-use
lemmate invite --server … --list             # id, and whether each is unused, expired, or spent
lemmate invite --server … --revoke <id>      # unused ones only
```

The same thing lives in the web client under **Account, password, tokens and invites…** (the command
palette, or the link on the vault-picker screen).

Opening the link shows the sign-up form; the recipient picks their own email and password. A few
consequences worth being deliberate about:

- **The link is a credential.** It is not bound to an email address, so whoever holds it can
  register — send it over a channel you would send a password over, and prefer `--expires-days`
  for anything that might sit in an inbox.
- **It cannot be reused or replayed.** Redeeming happens in the same transaction that creates the
  account, so two people opening one link at the same moment still produce exactly one account.
- **A spent invite is kept, not deleted**, and `--list` names the account it created. That is the
  record of how each person got in, which is why `--revoke` refuses a used one (`409`).
- An invited account is **never an admin**.

### Passwords

There is no reset-by-email flow, so an admin is the recovery path:

```sh
lemmate passwd --server https://notes.example.org                       # your own; asks for the current one
lemmate passwd --server … --email someone@example.org                   # admin reset; asks for nothing else
```

Either way **every other session of that account is revoked** — a password change that left old
sessions alive would not actually revoke anything. Changing your own password keeps the session
you did it from; an admin reset signs the target out everywhere, including any `lemmate sync`
running on their machines, which then need `lemmate login` again.

---

## (e) Security notes

**Never run `--no-auth` on a network.** It is a development switch: it sets
`AuthMode::Disabled`, and every request — REST, relay frames, attachment uploads — is then
treated as a local owner with no token at all. Anyone who can reach the port owns every vault.
The server logs a warning at startup when it is on. The same applies to `LEMMATE_NO_AUTH`, which
is the same switch by another name; keep it out of Compose files and environment files.

**The first registered account is the admin.** Registration is allowed when *any* of these hold
(`crates/server/src/auth.rs`): the user table is empty, `--allow-registration` is set, the
request carries the session of an existing admin, or it carries a valid invite. So on a fresh
server the very first `POST /api/v1/auth/register` — i.e. `lemmate login --register` — succeeds
without credentials and creates an admin; every later attempt is `403` unless one of the other
three conditions applies.

Deploy and register immediately. Between starting the server and your first `lemmate login --register`,
whoever reaches the URL first becomes the admin.

**`--allow-registration` opens self-service signup to the whole internet.** Leave it off for a
personal or small-team server and let people in one at a time, either by having the admin create
the account outright (an admin's `POST /api/v1/auth/register` creates the user without logging
the admin out of their own session) or with an invite (§d above). Turn the flag on only behind
something else that restricts who can reach the server. The only validation on a new account is
that the email contains `@` and the password is at least 8 characters.

**Quarto renders run what a note's front matter asks for.** Code cells never run — every
render passes `--no-execute` — but front matter can name Lua filters, and files to include in
the output, and Quarto honours both. On a shared server that means anyone who can edit a note
can run Lua and read files as the server's user, inside the container. If that is more than
you want to allow, set `LEMMATE_DISABLE_QUARTO=true` (or build without Quarto, above): renders
then answer 501 and the app says rendering is unavailable.

**Other things worth doing:**

- Set `--secure-cookies` on every HTTPS deployment (see above).
- Sessions are opaque bearer tokens, hashed at rest. `lemmate logout --server <url>` forgets the
  local copy. Access tokens (`lmt_…`) are hashed the same way; give each script its own, scoped
  to the vaults it needs and read only where it can be, and revoke it when it is done. None of
  them carries admin rights.
- There is no end-to-end encryption by design (SPEC §15) — the server reads note content in
  order to index, search, and share it. Encrypt the disk or volume if that matters.
- Vault roles (owner / editor / viewer) are enforced on both REST and every relay frame, but a
  vault that nobody owns yet is claimed by the first user who syncs it. On a shared server,
  create and claim your vaults before handing out accounts.
