// node:sqlite driver for RelayStore — the self-hosted entry and the
// endpoint tests (':memory:'). Requires Node ≥ 24 (node:sqlite). Shares
// relay/schema.sql verbatim with the D1 driver.

import { DatabaseSync } from 'node:sqlite'
import type { NewRegistration, RegistrationRecord, RelayStore } from '../app.ts'
import type { ApnsEnvironment } from '../types.ts'
import { LIMITS } from '../validate.ts'

export class SqliteStore implements RelayStore {
  private db: DatabaseSync

  constructor(path: string, schemaSQL: string) {
    this.db = new DatabaseSync(path)
    this.db.exec('PRAGMA journal_mode = WAL')
    this.db.exec(schemaSQL)
  }

  async createRegistration(r: NewRegistration): Promise<void> {
    this.db.prepare(
      `INSERT INTO registrations
         (routing_id, secret_hash, platform, token_kind, apns_environment, apns_token, created_at, last_seen_at)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?)`)
      .run(r.routingID, r.secretHash, r.platform, r.tokenKind,
           r.apnsEnvironment, r.apnsToken, r.createdAt, r.lastSeenAt)
  }

  async findRegistration(routingID: string): Promise<RegistrationRecord | null> {
    const row = this.db.prepare('SELECT * FROM registrations WHERE routing_id = ?')
      .get(routingID) as Record<string, unknown> | undefined
    if (!row) return null
    return {
      routingID: row.routing_id as string,
      secretHash: row.secret_hash as string,
      apnsEnvironment: row.apns_environment as ApnsEnvironment,
      apnsToken: row.apns_token as string,
      lastSeenAt: row.last_seen_at as number,
    }
  }

  async deleteRegistration(routingID: string): Promise<void> {
    this.db.prepare('DELETE FROM registrations WHERE routing_id = ?').run(routingID)
  }

  async renewRegistration(routingID: string, ts: number, apnsToken?: string): Promise<void> {
    this.db.prepare(
      'UPDATE registrations SET last_seen_at = ?, apns_token = COALESCE(?, apns_token) WHERE routing_id = ?')
      .run(ts, apnsToken ?? null, routingID)
  }

  async lastSentAt(routingID: string, collapseKey: string): Promise<number | null> {
    const row = this.db.prepare(
      'SELECT sent_at FROM send_log WHERE routing_id = ? AND collapse_key = ?')
      .get(routingID, collapseKey) as { sent_at: number } | undefined
    return row?.sent_at ?? null
  }

  async markSent(routingID: string, collapseKey: string, ts: number): Promise<void> {
    this.db.prepare(
      `INSERT INTO send_log (routing_id, collapse_key, sent_at) VALUES (?, ?, ?)
       ON CONFLICT(routing_id, collapse_key) DO UPDATE SET sent_at = excluded.sent_at`)
      .run(routingID, collapseKey, ts)
  }

  async bumpHourly(key: string, bucket: number): Promise<number> {
    const row = this.db.prepare(
      `INSERT INTO hourly_counts (routing_id, hour_bucket, count) VALUES (?, ?, 1)
       ON CONFLICT(routing_id, hour_bucket) DO UPDATE SET count = count + 1
       RETURNING count`)
      .get(key, bucket) as { count: number } | undefined
    return row?.count ?? 1
  }

  async purge(now: number): Promise<void> {
    this.db.prepare('DELETE FROM registrations WHERE last_seen_at < ?')
      .run(now - LIMITS.registrationTTLSeconds)
    this.db.prepare('DELETE FROM send_log WHERE sent_at < ?').run(now - 86400)
    this.db.prepare('DELETE FROM hourly_counts WHERE hour_bucket < ?')
      .run(Math.floor(now / 3600) - 2)
  }
}
