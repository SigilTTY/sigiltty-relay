// Endpoint-level pins: the Hono app over an in-memory node:sqlite store and
// a scripted APNs sender — the full protocol flow (PROTOCOL §3) without
// Workers or Apple. Real APNs and D1 stay integration-tested.

import { readFileSync } from 'node:fs'
import { describe, expect, test } from 'vitest'
import type { ApnsSender, SendOutcome } from '../src/apns.ts'
import { createRelay } from '../src/app.ts'
import { SqliteStore } from '../src/store/sqlite.ts'
import { LIMITS } from '../src/validate.ts'

const SCHEMA = readFileSync(new URL('../schema.sql', import.meta.url), 'utf8')
const TOKEN = 'a'.repeat(64)
const CIPHERTEXT = 'A'.repeat(64)

class ScriptedApns implements ApnsSender {
  outcome: SendOutcome = 'sent'
  sent: Array<{ environment: string; collapseKey: string; ciphertext: string }> = []
  async send(environment: string, _token: string, collapseKey: string, ciphertext: string): Promise<SendOutcome> {
    this.sent.push({ environment, collapseKey, ciphertext })
    return this.outcome
  }
}

function makeApp() {
  const apns = new ScriptedApns()
  const app = createRelay({ store: new SqliteStore(':memory:', SCHEMA), apns })
  return { app, apns }
}

type App = ReturnType<typeof makeApp>['app']

async function post(app: App, path: string, body: unknown): Promise<Response> {
  return app.request(path, {
    method: 'POST',
    body: JSON.stringify(body),
    headers: { 'content-type': 'application/json' },
  })
}

async function register(app: App): Promise<{ routingID: string; secret: string }> {
  const response = await post(app, '/v1/register', {
    platform: 'ios', tokenKind: 'alert', apnsEnvironment: 'sandbox', apnsToken: TOKEN,
  })
  expect(response.status).toBe(200)
  return response.json()
}

function entry(reg: { routingID: string; secret: string }, overrides: Record<string, unknown> = {}) {
  return {
    routingID: reg.routingID,
    secret: reg.secret,
    collapseKey: 'herdr-server-w1:p4',
    ts: Math.floor(Date.now() / 1000),
    ciphertext: CIPHERTEXT,
    ...overrides,
  }
}

async function results(app: App, entries: unknown[]): Promise<string[]> {
  const response = await post(app, '/v1/events', { events: entries })
  expect(response.status).toBe(200)
  return (await response.json() as { results: string[] }).results
}

describe('register + renew', () => {
  test('roundtrip: register, renew with the right secret, reject the wrong one', async () => {
    const { app } = makeApp()
    const reg = await register(app)
    expect(reg.routingID).toMatch(/^[0-9a-f-]{36}$/)

    const renewed = await post(app, '/v1/renew', { routingID: reg.routingID, secret: reg.secret })
    expect(renewed.status).toBe(204)
    const badSecret = await post(app, '/v1/renew', { routingID: reg.routingID, secret: 'x'.repeat(44) })
    expect(badSecret.status).toBe(401)
    const unknown = await post(app, '/v1/renew', { routingID: crypto.randomUUID(), secret: reg.secret })
    expect(unknown.status).toBe(404)
  })

  test('registration is IP-rate-limited', async () => {
    const { app } = makeApp()
    for (let i = 0; i < LIMITS.registerPerIPHourlyMax; i++) await register(app)
    const over = await post(app, '/v1/register', {
      platform: 'ios', tokenKind: 'alert', apnsEnvironment: 'sandbox', apnsToken: TOKEN,
    })
    expect(over.status).toBe(429)
  })
})

describe('events', () => {
  test('delivers to APNs with the registration environment and the entry collapse key', async () => {
    const { app, apns } = makeApp()
    const reg = await register(app)
    expect(await results(app, [entry(reg)])).toEqual(['sent'])
    expect(apns.sent).toEqual([
      { environment: 'sandbox', collapseKey: 'herdr-server-w1:p4', ciphertext: CIPHERTEXT },
    ])
  })

  test('per-pane clock: immediate repeat is limited, another pane is not', async () => {
    const { app } = makeApp()
    const reg = await register(app)
    expect(await results(app, [entry(reg)])).toEqual(['sent'])
    expect(await results(app, [entry(reg)])).toEqual(['rateLimited'])
    expect(await results(app, [entry(reg, { collapseKey: 'herdr-server-w2:p1' })])).toEqual(['sent'])
  })

  test('sendFailed does not advance the per-pane clock — the retry goes through', async () => {
    const { app, apns } = makeApp()
    const reg = await register(app)
    apns.outcome = 'sendFailed'
    expect(await results(app, [entry(reg)])).toEqual(['sendFailed'])
    apns.outcome = 'sent'
    expect(await results(app, [entry(reg)])).toEqual(['sent'])
  })

  test('a gone device token deletes the registration', async () => {
    const { app, apns } = makeApp()
    const reg = await register(app)
    apns.outcome = 'gone'
    expect(await results(app, [entry(reg)])).toEqual(['unknownRouting'])
    const renewAfter = await post(app, '/v1/renew', { routingID: reg.routingID, secret: reg.secret })
    expect(renewAfter.status).toBe(404)
  })

  test('bad secret and stale timestamp fail per-entry without blocking the batch', async () => {
    const { app } = makeApp()
    const reg = await register(app)
    const stale = entry(reg, { ts: Math.floor(Date.now() / 1000) - LIMITS.replayWindowSeconds - 10 })
    const wrongSecret = entry(reg, { secret: 'x'.repeat(44), collapseKey: 'herdr-server-w3:p1' })
    expect(await results(app, [stale, wrongSecret, entry(reg)])).toEqual(['invalid', 'badSecret', 'sent'])
  })

  test('hourly per-routing cap closes after 30 sends', async () => {
    const { app } = makeApp()
    const reg = await register(app)
    for (let i = 0; i < LIMITS.perRoutingHourlyMax; i++) {
      expect(await results(app, [entry(reg, { collapseKey: `herdr-server-w1:p${i}` })])).toEqual(['sent'])
    }
    expect(await results(app, [entry(reg, { collapseKey: 'herdr-server-w9:p9' })])).toEqual(['rateLimited'])
  })
})
