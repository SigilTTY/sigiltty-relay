// Pins the pure validation layer against PROTOCOL.md §3/§7. Endpoint glue,
// D1 and APNs stay integration-tested (wrangler dev + real devices).

import { describe, expect, test } from 'vitest'
import { LIMITS, parseEventsEnvelope, parseRegister, parseRenew, validateEntry } from '../src/validate.ts'

const NOW = 1_800_000_000
const TOKEN = 'a'.repeat(64)
const CIPHERTEXT = 'A'.repeat(64)

describe('parseRegister', () => {
  const valid = { platform: 'ios', tokenKind: 'alert', apnsEnvironment: 'production', apnsToken: TOKEN }

  test('accepts a valid request and lowercases the token', () => {
    const req = parseRegister({ ...valid, apnsToken: TOKEN.toUpperCase() })
    expect(req?.apnsToken).toBe(TOKEN)
    expect(req?.apnsEnvironment).toBe('production')
  })

  test('accepts sandbox, rejects unknown environments', () => {
    expect(parseRegister({ ...valid, apnsEnvironment: 'sandbox' })).not.toBeNull()
    expect(parseRegister({ ...valid, apnsEnvironment: 'staging' })).toBeNull()
  })

  test('rejects non-ios platform and reserved token kinds', () => {
    expect(parseRegister({ ...valid, platform: 'macos' })).toBeNull()
    // liveActivity is reserved, not accepted in v1 (PROTOCOL §3.1).
    expect(parseRegister({ ...valid, tokenKind: 'liveActivity' })).toBeNull()
  })

  test('rejects malformed tokens', () => {
    expect(parseRegister({ ...valid, apnsToken: 'a'.repeat(63) })).toBeNull()
    expect(parseRegister({ ...valid, apnsToken: 'z'.repeat(64) })).toBeNull()
    expect(parseRegister({ ...valid, apnsToken: 42 })).toBeNull()
    expect(parseRegister(null)).toBeNull()
  })
})

describe('parseRenew', () => {
  test('accepts with and without a replacement token', () => {
    expect(parseRenew({ routingID: crypto.randomUUID(), secret: 's'.repeat(44) })).not.toBeNull()
    const renewed = parseRenew({ routingID: crypto.randomUUID(), secret: 's'.repeat(44), apnsToken: TOKEN.toUpperCase() })
    expect(renewed?.apnsToken).toBe(TOKEN)
  })

  test('rejects a malformed replacement token instead of ignoring it', () => {
    expect(parseRenew({ routingID: crypto.randomUUID(), secret: 's'.repeat(44), apnsToken: 'nope' })).toBeNull()
  })
})

describe('parseEventsEnvelope', () => {
  test('caps the batch at the protocol limit', () => {
    expect(parseEventsEnvelope({ events: Array(LIMITS.eventsPerPost).fill({}) })).toHaveLength(16)
    expect(parseEventsEnvelope({ events: Array(LIMITS.eventsPerPost + 1).fill({}) })).toBeNull()
    expect(parseEventsEnvelope({ events: [] })).toBeNull()
    expect(parseEventsEnvelope({})).toBeNull()
  })
})

describe('validateEntry', () => {
  const valid = {
    routingID: crypto.randomUUID(),
    secret: 's'.repeat(44),
    collapseKey: `herdr-${crypto.randomUUID()}-w1:p4`,
    ts: NOW,
    ciphertext: CIPHERTEXT,
  }

  test('accepts a valid entry and floors the timestamp', () => {
    expect(validateEntry({ ...valid, ts: NOW + 0.7 }, NOW)?.ts).toBe(NOW)
  })

  test('enforces the replay window on both sides', () => {
    expect(validateEntry({ ...valid, ts: NOW - LIMITS.replayWindowSeconds }, NOW)).not.toBeNull()
    expect(validateEntry({ ...valid, ts: NOW - LIMITS.replayWindowSeconds - 1 }, NOW)).toBeNull()
    expect(validateEntry({ ...valid, ts: NOW + LIMITS.replayWindowSeconds + 1 }, NOW)).toBeNull()
  })

  test('bounds the collapse key: printable ASCII, ≤64 chars', () => {
    expect(validateEntry({ ...valid, collapseKey: 'x'.repeat(64) }, NOW)).not.toBeNull()
    expect(validateEntry({ ...valid, collapseKey: 'x'.repeat(65) }, NOW)).toBeNull()
    expect(validateEntry({ ...valid, collapseKey: 'herdr-… ' }, NOW)).toBeNull()
    expect(validateEntry({ ...valid, collapseKey: '' }, NOW)).toBeNull()
  })

  test('bounds the ciphertext: base64, HPKE minimum, APNs headroom', () => {
    expect(validateEntry({ ...valid, ciphertext: 'A'.repeat(43) }, NOW)).toBeNull()
    expect(validateEntry({ ...valid, ciphertext: 'A'.repeat(3501) }, NOW)).toBeNull()
    expect(validateEntry({ ...valid, ciphertext: 'not base64!!' }, NOW)).toBeNull()
  })
})
