// Runtime-agnostic relay application (Hono): the same app serves the
// Cloudflare Workers entry (src/index.ts) and the self-hosted Node entry
// (src/node.ts). Runtime specifics live only in the entries; persistence
// hides behind RelayStore, APNs behind ApnsSender. Zero-knowledge by
// construction: nothing here parses, stores or logs event content — only
// routing metadata ever reaches the store.

import { Hono } from 'hono'
import { bodyLimit } from 'hono/body-limit'
import type { ApnsSender } from './apns.ts'
import type { ApnsEnvironment, EventEntry, EventResult } from './types.ts'
import { base64FromBytes, sha256Hex, timingSafeEqual } from './util.ts'
import { LIMITS, parseEventsEnvelope, parseRegister, parseRenew, validateEntry } from './validate.ts'

export interface RegistrationRecord {
  routingID: string
  secretHash: string
  apnsEnvironment: ApnsEnvironment
  apnsToken: string
  lastSeenAt: number
}

export interface NewRegistration extends RegistrationRecord {
  platform: string
  tokenKind: string
  createdAt: number
}

/// Persistence seam — D1 on Workers, node:sqlite when self-hosted. Both
/// drivers share relay/schema.sql verbatim; policy (TTLs, limits) stays
/// here so the drivers are dumb rows-in-rows-out adapters.
export interface RelayStore {
  createRegistration(registration: NewRegistration): Promise<void>
  findRegistration(routingID: string): Promise<RegistrationRecord | null>
  deleteRegistration(routingID: string): Promise<void>
  renewRegistration(routingID: string, ts: number, apnsToken?: string): Promise<void>
  lastSentAt(routingID: string, collapseKey: string): Promise<number | null>
  markSent(routingID: string, collapseKey: string, ts: number): Promise<void>
  /// Increments and returns the counter for (key, hour bucket).
  bumpHourly(key: string, bucket: number): Promise<number>
  purge(now: number): Promise<void>
}

export interface RelayDeps {
  store: RelayStore
  apns: ApnsSender
  clientIP?: (headers: Headers) => string
  now?: () => number
}

function defaultClientIP(headers: Headers): string {
  return (
    headers.get('cf-connecting-ip')
    ?? headers.get('x-forwarded-for')?.split(',')[0]?.trim()
    ?? 'unknown'
  )
}

export function createRelay(deps: RelayDeps): Hono {
  const now = deps.now ?? (() => Math.floor(Date.now() / 1000))
  const clientIP = deps.clientIP ?? defaultClientIP
  const app = new Hono()

  app.use(bodyLimit({
    maxSize: 64 * 1024,
    onError: (c) => c.json({ error: 'payloadTooLarge' }, 413),
  }))
  app.notFound((c) => c.json({ error: 'notFound' }, 404))

  app.post('/v1/register', async (c) => {
    let body: unknown
    try {
      body = await c.req.json()
    } catch {
      return c.json({ error: 'malformedJSON' }, 400)
    }
    const request = parseRegister(body)
    if (!request) return c.json({ error: 'invalid' }, 400)
    const ts = now()
    const ip = clientIP(c.req.raw.headers)
    if ((await deps.store.bumpHourly(`ip:${ip}`, Math.floor(ts / 3600))) > LIMITS.registerPerIPHourlyMax) {
      return c.json({ error: 'rateLimited' }, 429)
    }
    const routingID = crypto.randomUUID()
    const secret = base64FromBytes(crypto.getRandomValues(new Uint8Array(32)))
    await deps.store.createRegistration({
      routingID,
      secretHash: await sha256Hex(secret),
      platform: request.platform,
      tokenKind: request.tokenKind,
      apnsEnvironment: request.apnsEnvironment,
      apnsToken: request.apnsToken,
      createdAt: ts,
      lastSeenAt: ts,
    })
    return c.json({ routingID, secret })
  })

  app.post('/v1/renew', async (c) => {
    let body: unknown
    try {
      body = await c.req.json()
    } catch {
      return c.json({ error: 'malformedJSON' }, 400)
    }
    const request = parseRenew(body)
    if (!request) return c.json({ error: 'invalid' }, 400)
    const ts = now()
    const row = await findLiveRegistration(deps.store, request.routingID, ts)
    if (!row) return c.json({ error: 'unknownRouting' }, 404)
    if (!timingSafeEqual(row.secretHash, await sha256Hex(request.secret))) {
      return c.json({ error: 'badSecret' }, 401)
    }
    await deps.store.renewRegistration(request.routingID, ts, request.apnsToken)
    return c.body(null, 204)
  })

  app.post('/v1/events', async (c) => {
    let body: unknown
    try {
      body = await c.req.json()
    } catch {
      return c.json({ error: 'malformedJSON' }, 400)
    }
    const entries = parseEventsEnvelope(body)
    if (!entries) return c.json({ error: 'invalid' }, 400)
    const ts = now()
    const results: EventResult[] = []
    for (const raw of entries) {
      const entry = validateEntry(raw, ts)
      results.push(entry ? await processEntry(entry, ts, deps) : 'invalid')
    }
    return c.json({ results })
  })

  return app
}

async function processEntry(entry: EventEntry, ts: number, deps: RelayDeps): Promise<EventResult> {
  const row = await findLiveRegistration(deps.store, entry.routingID, ts)
  if (!row) return 'unknownRouting'
  if (!timingSafeEqual(row.secretHash, await sha256Hex(entry.secret))) return 'badSecret'

  const last = await deps.store.lastSentAt(entry.routingID, entry.collapseKey)
  if (last !== null && ts - last < LIMITS.perPaneIntervalSeconds) return 'rateLimited'
  if ((await deps.store.bumpHourly(entry.routingID, Math.floor(ts / 3600))) > LIMITS.perRoutingHourlyMax) {
    return 'rateLimited'
  }

  const outcome = await deps.apns.send(
    row.apnsEnvironment, row.apnsToken, entry.collapseKey, entry.ciphertext, entry.ts)
  if (outcome === 'gone') {
    // Permanently dead device token (PROTOCOL §3.3): drop the registration.
    await deps.store.deleteRegistration(entry.routingID)
    return 'unknownRouting'
  }
  if (outcome === 'sendFailed') return 'sendFailed'
  // Only a successful send advances the per-pane clock, so a watcher retry
  // after sendFailed is never blocked by its own failed attempt.
  await deps.store.markSent(entry.routingID, entry.collapseKey, ts)
  return 'sent'
}

/// Fetches a registration, lazily deleting it when the sliding 30-day TTL
/// has lapsed — the periodic purge is a sweep, not the enforcement point.
async function findLiveRegistration(
  store: RelayStore, routingID: string, ts: number,
): Promise<RegistrationRecord | null> {
  const row = await store.findRegistration(routingID)
  if (!row) return null
  if (ts - row.lastSeenAt > LIMITS.registrationTTLSeconds) {
    await store.deleteRegistration(routingID)
    return null
  }
  return row
}
