// Pins the pure APNs pieces: JWT signing input, PEM→DER (verified by a real
// WebCrypto import + sign), and the fallback payload's size headroom.
// The HTTP send path stays integration-tested against real APNs.

import { generateKeyPairSync } from 'node:crypto'
import { describe, expect, test } from 'vitest'
import { buildPayload, jwtSigningInput, pemToDer } from '../src/apns.ts'

function decodeBase64url(part: string): unknown {
  const b64 = part.replace(/-/g, '+').replace(/_/g, '/')
  return JSON.parse(Buffer.from(b64, 'base64').toString('utf8'))
}

describe('jwtSigningInput', () => {
  test('encodes an ES256 header and iss/iat claims', () => {
    const input = jwtSigningInput('TEAM123456', 'KEY1234567', 1_800_000_000)
    const [header, claims, extra] = input.split('.')
    expect(extra).toBeUndefined()
    expect(decodeBase64url(header!)).toEqual({ alg: 'ES256', kid: 'KEY1234567' })
    expect(decodeBase64url(claims!)).toEqual({ iss: 'TEAM123456', iat: 1_800_000_000 })
  })
})

describe('pemToDer', () => {
  test('yields a PKCS8 body WebCrypto can import and sign with', async () => {
    const { privateKey } = generateKeyPairSync('ec', { namedCurve: 'P-256' })
    const pem = privateKey.export({ type: 'pkcs8', format: 'pem' }) as string
    const key = await crypto.subtle.importKey(
      'pkcs8', pemToDer(pem), { name: 'ECDSA', namedCurve: 'P-256' }, false, ['sign'])
    const signature = await crypto.subtle.sign(
      { name: 'ECDSA', hash: 'SHA-256' }, key, new TextEncoder().encode('probe'))
    // JOSE ES256: raw r‖s, 64 bytes.
    expect(new Uint8Array(signature).length).toBe(64)
  })
})

describe('buildPayload', () => {
  test('carries the envelope with generic fallback copy and mutable-content', () => {
    const payload = JSON.parse(buildPayload('QUJD')) as Record<string, any>
    expect(payload.aps.alert.title).toBe('SigilTTY')
    expect(payload.aps['mutable-content']).toBe(1)
    expect(payload.e).toBe('QUJD')
    expect(payload.v).toBe(1)
  })

  test('stays under the 4096-byte APNs cap at the maximum ciphertext size', () => {
    const maximal = buildPayload('A'.repeat(3500))
    expect(new TextEncoder().encode(maximal).length).toBeLessThanOrEqual(4096)
  })
})
