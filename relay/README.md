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
pnpm dev         # wrangler dev
```

## Prerequisite: an APNs auth key

Apple Developer portal → Certificates, Identifiers & Profiles → **Keys** → new key with **Apple Push Notifications service (APNs)** enabled. The `.p8` downloads **once** — keep it; note the **Key ID** shown next to it and the **Team ID** (portal header, same as the Xcode project's `DEVELOPMENT_TEAM`). One token-auth key serves both the sandbox and production APNs hosts.

## Deploy: Cloudflare Workers

```bash
pnpm wrangler login                     # interactive, once per machine
pnpm wrangler d1 create sigiltty-relay  # paste the database_id into wrangler.toml
pnpm wrangler d1 execute sigiltty-relay --file=schema.sql --remote
# fill APNS_TEAM_ID / APNS_KEY_ID in wrangler.toml (APNS_TOPIC is already the bundle ID)
pnpm wrangler secret put APNS_PRIVATE_KEY   # paste the whole .p8 file contents
pnpm run deploy
```

Use `pnpm run deploy`, not `pnpm deploy` — pnpm has a built-in `deploy` command that swallows the script. `pnpm run deploy --dry-run` bundles without uploading (a useful preflight; needs no login).

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

## Deploy: self-hosted (Node ≥ 24)

```bash
APNS_TEAM_ID=… APNS_KEY_ID=… APNS_PRIVATE_KEY_FILE=/path/to/AuthKey.p8 \
PORT=8788 DB_PATH=/var/lib/sigiltty-relay/relay.sqlite3 \
node src/node.ts
```

Runs the `.ts` entry directly (native type stripping). `APNS_PRIVATE_KEY` (inline PEM) is accepted instead of `_FILE`; `APNS_TOPIC` defaults to `com.sigiltty.shell`. Purge runs at boot and daily. Terminate TLS in front of it (reverse proxy) — device registrations carry APNs tokens.

**APNs is HTTP/2-only**: the Node entry routes sends through undici with `allowH2` — plain `fetch` would be refused by Apple. The Workers entry relies on the platform fetch, which already speaks h2.

Development-signed app builds register with `"apnsEnvironment": "sandbox"`; the relay picks the APNs host per registration, so one deployment serves both environments.
