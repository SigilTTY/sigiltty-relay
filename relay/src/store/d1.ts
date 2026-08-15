// D1 driver for RelayStore — the Cloudflare Workers entry only. Shares
// relay/schema.sql verbatim with the node:sqlite driver; keep the SQL in
// the two drivers textually identical wherever the API allows.

import type { NewRegistration, RegistrationRecord, RelayStore } from '../app.ts'
import type { ApnsEnvironment } from '../types.ts'
import { LIMITS } from '../validate.ts'

interface RegistrationRow {
  routing_id: string
  secret_hash: string
  apns_environment: ApnsEnvironment
  apns_token: string
  last_seen_at: number
}

export class D1Store implements RelayStore {
  constructor(private db: D1Database) {}

  async createRegistration(r: NewRegistration): Promise<void> {
    await this.db.prepare(
      `INSERT INTO registrations
         (routing_id, secret_hash, platform, token_kind, apns_environment, apns_token, created_at, last_seen_at)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?)`)
      .bind(r.routingID, r.secretHash, r.platform, r.tokenKind,
            r.apnsEnvironment, r.apnsToken, r.createdAt, r.lastSeenAt)
      .run()
  }

  async findRegistration(routingID: string): Promise<RegistrationRecord | null> {
    const row = await this.db.prepare('SELECT * FROM registrations WHERE routing_id = ?')
      .bind(routingID)
      .first<RegistrationRow>()
    if (!row) return null
    return {
      routingID: row.routing_id,
      secretHash: row.secret_hash,
      apnsEnvironment: row.apns_environment,
      apnsToken: row.apns_token,
      lastSeenAt: row.last_seen_at,
    }
  }

  async deleteRegistration(routingID: string): Promise<void> {
    await this.db.prepare('DELETE FROM registrations WHERE routing_id = ?').bind(routingID).run()
  }

  async renewRegistration(routingID: string, ts: number, apnsToken?: string): Promise<void> {
    await this.db.prepare(
      'UPDATE registrations SET last_seen_at = ?, apns_token = COALESCE(?, apns_token) WHERE routing_id = ?')
      .bind(ts, apnsToken ?? null, routingID)
      .run()
  }

  async lastSentAt(routingID: string, collapseKey: string): Promise<number | null> {
    const row = await this.db.prepare(
      'SELECT sent_at FROM send_log WHERE routing_id = ? AND collapse_key = ?')
      .bind(routingID, collapseKey)
      .first<{ sent_at: number }>()
    return row?.sent_at ?? null
  }

  async markSent(routingID: string, collapseKey: string, ts: number): Promise<void> {
    await this.db.prepare(
      `INSERT INTO send_log (routing_id, collapse_key, sent_at) VALUES (?, ?, ?)
       ON CONFLICT(routing_id, collapse_key) DO UPDATE SET sent_at = excluded.sent_at`)
      .bind(routingID, collapseKey, ts)
      .run()
  }

  async bumpHourly(key: string, bucket: number): Promise<number> {
    const row = await this.db.prepare(
      `INSERT INTO hourly_counts (routing_id, hour_bucket, count) VALUES (?, ?, 1)
       ON CONFLICT(routing_id, hour_bucket) DO UPDATE SET count = count + 1
       RETURNING count`)
      .bind(key, bucket)
      .first<{ count: number }>()
    return row?.count ?? 1
  }

  async purge(now: number): Promise<void> {
    await this.db.batch([
      this.db.prepare('DELETE FROM registrations WHERE last_seen_at < ?')
        .bind(now - LIMITS.registrationTTLSeconds),
      this.db.prepare('DELETE FROM send_log WHERE sent_at < ?').bind(now - 86400),
      this.db.prepare('DELETE FROM hourly_counts WHERE hour_bucket < ?')
        .bind(Math.floor(now / 3600) - 2),
    ])
  }
}
