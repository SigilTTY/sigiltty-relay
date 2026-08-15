// Request validation, pure and unit-tested. LIMITS mirrors the table in
// docs/PROTOCOL.md §7 — change them together.

import type { EventEntry, RegisterRequest, RenewRequest } from './types.ts'

export const LIMITS = {
  replayWindowSeconds: 300,
  collapseKeyMaxLength: 64,
  eventsPerPost: 16,
  perPaneIntervalSeconds: 60,
  perRoutingHourlyMax: 30,
  registerPerIPHourlyMax: 10,
  registrationTTLSeconds: 30 * 24 * 3600,
} as const

// Device tokens are currently 32 bytes (64 hex chars); Apple reserves the
// right to grow them, so accept a generous upper bound.
const APNS_TOKEN = /^[0-9a-fA-F]{64,200}$/
const BASE64 = /^[A-Za-z0-9+/]+={0,2}$/
// Printable ASCII only — the collapse key travels as an APNs header.
const COLLAPSE_KEY = /^[\x21-\x7e]{1,64}$/
// HPKE seal output is enc(32 B) ‖ ct(≥16 B tag); the upper bound keeps the
// final APNs payload (fallback aps + envelope) under the 4096-byte cap.
const CIPHERTEXT_MIN = 44
const CIPHERTEXT_MAX = 3500

function record(body: unknown): Record<string, unknown> | null {
  return typeof body === 'object' && body !== null ? (body as Record<string, unknown>) : null
}

export function parseRegister(body: unknown): RegisterRequest | null {
  const b = record(body)
  if (!b) return null
  if (b.platform !== 'ios' || b.tokenKind !== 'alert') return null
  if (b.apnsEnvironment !== 'production' && b.apnsEnvironment !== 'sandbox') return null
  if (typeof b.apnsToken !== 'string' || !APNS_TOKEN.test(b.apnsToken)) return null
  return {
    platform: 'ios',
    tokenKind: 'alert',
    apnsEnvironment: b.apnsEnvironment,
    apnsToken: b.apnsToken.toLowerCase(),
  }
}

export function parseRenew(body: unknown): RenewRequest | null {
  const b = record(body)
  if (!b) return null
  if (typeof b.routingID !== 'string' || b.routingID.length < 8 || b.routingID.length > 64) return null
  if (typeof b.secret !== 'string' || b.secret.length < 8 || b.secret.length > 128) return null
  if (b.apnsToken !== undefined && (typeof b.apnsToken !== 'string' || !APNS_TOKEN.test(b.apnsToken))) return null
  return {
    routingID: b.routingID,
    secret: b.secret,
    ...(typeof b.apnsToken === 'string' ? { apnsToken: b.apnsToken.toLowerCase() } : {}),
  }
}

/// Envelope check only — entries are validated one by one so a bad entry
/// yields a per-entry "invalid" instead of failing the whole POST.
export function parseEventsEnvelope(body: unknown): unknown[] | null {
  const b = record(body)
  if (!b || !Array.isArray(b.events)) return null
  if (b.events.length < 1 || b.events.length > LIMITS.eventsPerPost) return null
  return b.events
}

export function validateEntry(entry: unknown, now: number): EventEntry | null {
  const e = record(entry)
  if (!e) return null
  if (typeof e.routingID !== 'string' || e.routingID.length < 8 || e.routingID.length > 64) return null
  if (typeof e.secret !== 'string' || e.secret.length < 8 || e.secret.length > 128) return null
  if (typeof e.collapseKey !== 'string' || !COLLAPSE_KEY.test(e.collapseKey)) return null
  if (typeof e.ts !== 'number' || !Number.isFinite(e.ts)) return null
  if (Math.abs(now - e.ts) > LIMITS.replayWindowSeconds) return null
  if (typeof e.ciphertext !== 'string' || !BASE64.test(e.ciphertext)) return null
  if (e.ciphertext.length < CIPHERTEXT_MIN || e.ciphertext.length > CIPHERTEXT_MAX) return null
  return {
    routingID: e.routingID,
    secret: e.secret,
    collapseKey: e.collapseKey,
    ts: Math.floor(e.ts),
    ciphertext: e.ciphertext,
  }
}
