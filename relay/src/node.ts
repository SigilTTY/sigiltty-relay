// Self-hosted entry — runnable directly on Node ≥ 24 (`node src/node.ts`,
// native type stripping). APNs is HTTP/2-only and Node's built-in fetch
// speaks HTTP/1.1, so sends go through undici with allowH2.
//
// Configuration (environment variables):
//   APNS_TEAM_ID, APNS_KEY_ID          required
//   APNS_PRIVATE_KEY | APNS_PRIVATE_KEY_FILE   required (.p8 contents | path)
//   APNS_TOPIC                         default com.sigiltty.shell
//   PORT                               default 8788
//   DB_PATH                            default ./sigiltty-relay.sqlite3

import { readFileSync } from 'node:fs'
import { serve } from '@hono/node-server'
import { Agent, fetch as undiciFetch } from 'undici'
import { createApnsSender, type FetchLike } from './apns.ts'
import { createRelay } from './app.ts'
import { SqliteStore } from './store/sqlite.ts'

function requireEnv(name: string): string {
  const value = process.env[name]
  if (!value) {
    console.error(`sigiltty-relay: missing required environment variable ${name}`)
    process.exit(1)
  }
  return value
}

const privateKeyPEM = process.env.APNS_PRIVATE_KEY
  ?? readFileSync(requireEnv('APNS_PRIVATE_KEY_FILE'), 'utf8')

const schema = readFileSync(new URL('../schema.sql', import.meta.url), 'utf8')
const store = new SqliteStore(process.env.DB_PATH ?? 'sigiltty-relay.sqlite3', schema)

const h2 = new Agent({ allowH2: true })
const h2Fetch: FetchLike = (url, init) =>
  undiciFetch(url, { ...init, dispatcher: h2 }) as unknown as Promise<Response>

const app = createRelay({
  store,
  apns: createApnsSender({
    teamID: requireEnv('APNS_TEAM_ID'),
    keyID: requireEnv('APNS_KEY_ID'),
    topic: process.env.APNS_TOPIC ?? 'com.sigiltty.shell',
    privateKeyPEM,
  }, h2Fetch),
})

const nowSeconds = () => Math.floor(Date.now() / 1000)
void store.purge(nowSeconds())
setInterval(() => void store.purge(nowSeconds()), 24 * 3600 * 1000).unref()

const port = Number(process.env.PORT ?? 8788)
serve({ fetch: app.fetch, port })
console.log(`sigiltty-relay listening on :${port}`)
