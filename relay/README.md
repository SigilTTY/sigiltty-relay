# @sigiltty/relay

Hono implementation of [docs/PROTOCOL.md](../docs/PROTOCOL.md): three endpoints (`/v1/register`, `/v1/renew`, `/v1/events`), registrations and rate limits in SQL, APNs HTTP/2 fan-out with ES256 provider tokens. The app (`src/app.ts`) is runtime-agnostic; two entries deploy it:

| Entry | Runtime | Store |
|---|---|---|
| `src/index.ts` | Cloudflare Workers (+ cron purge) | D1 |
| `src/node.ts` | Node ≥ 24, self-hosted | `node:sqlite` |

Both stores apply the same `schema.sql`.

## Development

```bash
pnpm install
pnpm test        # vitest — validation, APNs pieces, endpoint flow over :memory: sqlite
pnpm typecheck   # both worlds: workers-types and @types/node
pnpm dev         # wrangler dev — needs .dev.vars (below)
```

Deployed secrets are not readable locally, so `wrangler dev` takes the same
names from an untracked `.dev.vars`:

```
APNS_TEAM_ID="…"
APNS_KEY_ID="…"
APNS_PRIVATE_KEY="-----BEGIN PRIVATE KEY-----\n…"
```

## Prerequisite: an APNs auth key

Apple Developer portal → Certificates, Identifiers & Profiles → **Keys** → new key with **Apple Push Notifications service (APNs)** enabled. The `.p8` downloads **once** — keep it; note the **Key ID** shown next to it and the **Team ID** (portal header, same as the Xcode project's `DEVELOPMENT_TEAM`). One token-auth key serves both the sandbox and production APNs hosts.

## Deploy: Cloudflare Workers

`wrangler.toml` is a template: it carries no account-specific value, so
these steps are what turn it into *your* deployment.

```bash
pnpm wrangler login                     # interactive, once per machine
pnpm wrangler d1 create sigiltty-relay  # paste the database_id into wrangler.toml
pnpm wrangler d1 execute sigiltty-relay --file=schema.sql --remote

# Credentials are secrets, never [vars] — see the note in wrangler.toml.
pnpm wrangler secret put APNS_TEAM_ID
pnpm wrangler secret put APNS_KEY_ID
pnpm wrangler secret put APNS_PRIVATE_KEY < /path/to/AuthKey_XXXXXXXXXX.p8
pnpm wrangler secret list                   # three names, no values
pnpm run deploy
```

Use `pnpm run deploy`, not `pnpm deploy` — pnpm has a built-in `deploy` command that swallows the script. `pnpm run deploy --dry-run` bundles without uploading (a useful preflight; needs no login).

Secrets take effect immediately and survive redeploys; only `wrangler secret delete` removes them. On the first `secret put` the Worker does not exist yet and wrangler offers to create it — answer yes.

### Keeping your deployment out of git

`database_id`, and `routes` if you serve on your own hostname, must be real
in the config wrangler reads — but committing them publishes your
deployment and forces every fork to undo it. Keep them in an untracked copy
instead (`wrangler.local.toml` is already ignored):

```bash
cp wrangler.toml wrangler.local.toml     # fill in the real values here
pnpm wrangler deploy --config wrangler.local.toml
```

The tracked `wrangler.toml` then stays at its placeholders. The cost is that
the two files drift, so re-copy after any change to bindings, triggers or
`compatibility_date`. Committing the real values instead is a defensible
choice for a repo nobody forks — neither a D1 UUID nor a hostname is a
credential — but it is not the default here.

## Verifying a deployment

Endpoints first (`$RELAY` = the deployed URL; add `--noproxy '*'` when testing a local Node entry through a proxying VPN client):

```bash
TOKEN=$(printf 'a%.0s' {1..64})   # syntactically valid, deliberately fake
REG=$(curl -s -X POST $RELAY/v1/register -H 'content-type: application/json' \
  -d '{"platform":"ios","tokenKind":"alert","apnsEnvironment":"sandbox","apnsToken":"'"$TOKEN"'"}')
echo "$REG"                        # → {"routingID":…,"secret":…}
```

Then the **APNs credential chain, without any device**: post one event for that fake token and read the pair of answers.

```bash
# … build an events POST with the routingID/secret from $REG, any collapseKey,
#    ts = now, ciphertext = 64 'A's (never decrypted — APNs rejects first) …
curl -s -X POST $RELAY/v1/events -H 'content-type: application/json' -d "$EVENTS"
curl -s -o /dev/null -w '%{http_code}\n' -X POST $RELAY/v1/renew \
  -H 'content-type: application/json' -d '{"routingID":…,"secret":…}'
```

| events result | renew after | Meaning |
|---|---|---|
| `unknownRouting` | `404` | ✅ APNs **accepted the JWT** and answered `BadDeviceToken` (expected — the token is fake), so the relay dropped the registration. Team ID, Key ID, `.p8` and topic are all correct. |
| `sendFailed` | `204` | ❌ APNs refused the provider token or the topic (`InvalidProviderToken` / `TopicDisallowed`), or the request never got out. Check the vars and the secret. |

Register fresh for each attempt: a successful check deletes the registration, so a second event on the same routing would return `unknownRouting` for the *other* reason. The relay logs no APNs reason strings — use `pnpm wrangler tail` while testing if you need the raw status.

A real banner on a real device needs a device token, which arrives with the app-side registration (SigilTTY P2).

## Reading logs and stored times

Every relay log line opens with an ISO-8601 UTC instant and a level:

```
2026-08-15T11:06:07.784Z INFO  sigiltty-relay listening on :8788
2026-08-15T11:06:07.785Z WARN  apns rejected: 403 InvalidProviderToken
```

UTC rather than local time: a Worker isolate has no meaningful timezone, and the line usually has to be lined up against an APNs response or a watcher log from elsewhere. `pnpm wrangler tail` streams them from the deployed Worker; the self-hosted entry writes them to stdout. Lines carry status codes, reasons and counts — never a device token, collapse key or envelope.

Times in the database are stored as unix seconds, since every read of one is a comparison, a range delete or a bucket key. Each is mirrored by a generated `*_utc` column rendering the same instant in the same format as the logs:

```bash
pnpm wrangler d1 execute sigiltty-relay --remote --command \
  "SELECT routing_id, apns_environment, created_at_utc, last_seen_at_utc FROM registrations"
```

| Table | Stored integer | Readable mirror |
|---|---|---|
| `registrations` | `created_at`, `last_seen_at` | `created_at_utc`, `last_seen_at_utc` |
| `send_log` | `sent_at` | `sent_at_utc` |
| `hourly_counts` | `hour_bucket` (hours since epoch) | `hour_start_utc` |

The mirrors are `VIRTUAL` generated columns — no stored bytes, computed on read, so they cannot drift from the integer beside them. Select columns explicitly rather than `SELECT *`: `registrations` holds live APNs device tokens, and command output tends to end up pasted somewhere.

**An existing database needs one migration.** `schema.sql` carries these columns for a fresh database, but `CREATE TABLE IF NOT EXISTS` never alters a table that already exists — re-applying it to a live relay changes nothing. Run once per database:

```bash
pnpm wrangler d1 execute sigiltty-relay --remote \
  --file=upgrades/0001_readable_timestamps.sql
```

Additive and reversible: no row is rewritten and `ALTER TABLE … DROP COLUMN` takes the columns back off. Re-running it fails on `duplicate column name`, which means it was already applied. Self-hosted equivalent: `sqlite3 /var/lib/sigiltty-relay/relay.sqlite3 < upgrades/0001_readable_timestamps.sql`.

## Deploy: self-hosted (Node ≥ 24)

```bash
APNS_TEAM_ID=… APNS_KEY_ID=… APNS_PRIVATE_KEY_FILE=/path/to/AuthKey.p8 \
PORT=8788 DB_PATH=/var/lib/sigiltty-relay/relay.sqlite3 \
node src/node.ts
```

Runs the `.ts` entry directly (native type stripping). `APNS_PRIVATE_KEY` (inline PEM) is accepted instead of `_FILE`; `APNS_TOPIC` defaults to `com.sigiltty.shell`. Purge runs at boot and daily. Terminate TLS in front of it (reverse proxy) — device registrations carry APNs tokens.

**APNs is HTTP/2-only**: the Node entry routes sends through undici with `allowH2` — plain `fetch` would be refused by Apple. The Workers entry relies on the platform fetch, which already speaks h2.

Development-signed app builds register with `"apnsEnvironment": "sandbox"`; the relay picks the APNs host per registration, so one deployment serves both environments.
