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

## Deploy: Cloudflare Workers

1. `wrangler d1 create sigiltty-relay` → paste the returned `database_id` into `wrangler.toml`.
2. `wrangler d1 execute sigiltty-relay --file=schema.sql --remote`
3. Fill `APNS_TEAM_ID` / `APNS_KEY_ID` in `wrangler.toml` (`APNS_TOPIC` is already `com.sigiltty.shell`).
4. `wrangler secret put APNS_PRIVATE_KEY` → paste the full contents of the `.p8` key file.
5. `pnpm deploy`

## Deploy: self-hosted (Node ≥ 24)

```bash
APNS_TEAM_ID=… APNS_KEY_ID=… APNS_PRIVATE_KEY_FILE=/path/to/AuthKey.p8 \
PORT=8788 DB_PATH=/var/lib/sigiltty-relay/relay.sqlite3 \
node src/node.ts
```

Runs the `.ts` entry directly (native type stripping). `APNS_PRIVATE_KEY` (inline PEM) is accepted instead of `_FILE`; `APNS_TOPIC` defaults to `com.sigiltty.shell`. Purge runs at boot and daily. Terminate TLS in front of it (reverse proxy) — device registrations carry APNs tokens.

**APNs is HTTP/2-only**: the Node entry routes sends through undici with `allowH2` — plain `fetch` would be refused by Apple. The Workers entry relies on the platform fetch, which already speaks h2.

Development-signed app builds register with `"apnsEnvironment": "sandbox"`; the relay picks the APNs host per registration, so one deployment serves both environments.
