// APNs HTTP/2 sender (docs/PROTOCOL.md §4). ES256 provider JWTs are signed
// with WebCrypto and cached inside the sender — APNs requires token reuse
// between 20 and 60 minutes, so entries construct ONE sender and keep it
// (module/process lifetime). The fetch implementation is injectable: on
// Workers the platform fetch speaks HTTP/2; on Node the entry must supply
// an h2-capable fetch (undici allowH2) — APNs rejects HTTP/1.1.

import { warn } from './log.ts'
import type { ApnsEnvironment } from './types.ts'
import { base64urlFromBytes, base64urlFromString } from './util.ts'

export type SendOutcome = 'sent' | 'gone' | 'sendFailed'

export interface ApnsSender {
  send(
    environment: ApnsEnvironment,
    deviceToken: string,
    collapseKey: string,
    ciphertext: string,
    eventTS: number,
  ): Promise<SendOutcome>
}

export interface ApnsCredentials {
  teamID: string
  keyID: string
  topic: string
  privateKeyPEM: string
}

/// Narrow fetch shape so Workers fetch and undici fetch both conform
/// without type gymnastics.
export type FetchLike = (
  url: string,
  init: { method: 'POST'; headers: Record<string, string>; body: string },
) => Promise<Response>

const HOSTS: Record<ApnsEnvironment, string> = {
  production: 'https://api.push.apple.com',
  sandbox: 'https://api.sandbox.push.apple.com',
}

const TOKEN_LIFETIME_SECONDS = 50 * 60
// Stale agent alerts must not surface hours later (PROTOCOL §4).
const EVENT_EXPIRY_SECONDS = 6 * 3600

// APNs reasons that mean the device token is permanently dead — the caller
// deletes the registration (PROTOCOL §3.3).
const GONE_REASONS = new Set(['BadDeviceToken', 'Unregistered', 'DeviceTokenNotForTopic'])

export function jwtSigningInput(teamID: string, keyID: string, iat: number): string {
  const header = base64urlFromString(JSON.stringify({ alg: 'ES256', kid: keyID }))
  const claims = base64urlFromString(JSON.stringify({ iss: teamID, iat }))
  return `${header}.${claims}`
}

export function pemToDer(pem: string): ArrayBuffer {
  const b64 = pem.replace(/-----[A-Z ]+-----/g, '').replace(/\s+/g, '')
  const bin = atob(b64)
  const bytes = new Uint8Array(bin.length)
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i)
  return bytes.buffer
}

/// The generic-fallback payload: real content is NSE-decrypted from `e`
/// (PROTOCOL §4/§5); this text shows only when the extension fails.
export function buildPayload(ciphertext: string): string {
  return JSON.stringify({
    aps: {
      alert: { title: 'SigilTTY', body: 'An agent needs attention.' },
      sound: 'default',
      'mutable-content': 1,
    },
    v: 1,
    e: ciphertext,
  })
}

export function createApnsSender(credentials: ApnsCredentials, fetchImpl?: FetchLike): ApnsSender {
  const doFetch: FetchLike = fetchImpl ?? ((url, init) => fetch(url, init))
  // Inferred, not the named CryptoKey type — @cloudflare/workers-types
  // declares CryptoKey as a bare value, so the name fails as a type there.
  let signingKey: Awaited<ReturnType<typeof crypto.subtle.importKey>> | null = null
  let cachedToken: { token: string; iat: number } | null = null

  async function bearer(now: number): Promise<string> {
    if (cachedToken && now - cachedToken.iat < TOKEN_LIFETIME_SECONDS) return cachedToken.token
    if (!signingKey) {
      signingKey = await crypto.subtle.importKey(
        'pkcs8', pemToDer(credentials.privateKeyPEM),
        { name: 'ECDSA', namedCurve: 'P-256' }, false, ['sign'])
    }
    const input = jwtSigningInput(credentials.teamID, credentials.keyID, now)
    // WebCrypto ECDSA emits raw r‖s — exactly the JOSE ES256 format.
    const signature = await crypto.subtle.sign(
      { name: 'ECDSA', hash: 'SHA-256' }, signingKey, new TextEncoder().encode(input))
    const token = `${input}.${base64urlFromBytes(new Uint8Array(signature))}`
    cachedToken = { token, iat: now }
    return token
  }

  return {
    async send(environment, deviceToken, collapseKey, ciphertext, eventTS) {
      const now = Math.floor(Date.now() / 1000)
      let response: Response
      try {
        response = await doFetch(`${HOSTS[environment]}/3/device/${deviceToken}`, {
          method: 'POST',
          headers: {
            authorization: `bearer ${await bearer(now)}`,
            'apns-topic': credentials.topic,
            'apns-push-type': 'alert',
            'apns-priority': '10',
            'apns-collapse-id': collapseKey,
            'apns-expiration': String(eventTS + EVENT_EXPIRY_SECONDS),
          },
          body: buildPayload(ciphertext),
        })
      } catch (error) {
        // Transport-level failure (TLS, DNS, no HTTP/2). Worth a line: an
        // opaque sendFailed with no reason is very expensive to debug.
        warn(`apns request failed: ${error instanceof Error ? error.message : String(error)}`)
        return 'sendFailed'
      }
      if (response.ok) return 'sent'
      const reason = await response
        .json()
        .then((b) => (b as { reason?: string } | null)?.reason)
        .catch(() => undefined)
      if (response.status === 410 || (reason !== undefined && GONE_REASONS.has(reason))) return 'gone'
      // Config-level rejections land here (InvalidProviderToken from a wrong
      // team/key, TopicDisallowed, ExpiredProviderToken from clock skew).
      // Status + reason only — never the token, never the envelope.
      warn(`apns rejected: ${response.status} ${reason ?? '(no reason)'}`)
      return 'sendFailed'
    },
  }
}
