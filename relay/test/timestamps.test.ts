// Timestamps are stored as unix seconds and rendered for humans in three
// places — src/log.ts, schema.sql's generated columns, and the watcher's
// watcher/src/timefmt.rs. These pin the renderings to each other: the point
// of the `*_utc` columns is that an operator can compare a relay log line
// against a database row (and against a watcher line from the server) without
// converting anything in their head, and that only holds while every side
// emits the same string for the same instant.

import { mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { DatabaseSync } from 'node:sqlite'
import { afterAll, describe, expect, test } from 'vitest'
import { isoAtOffset, isoFromUnix } from '../src/log.ts'
import { SqliteStore } from '../src/store/sqlite.ts'

const SCHEMA = readFileSync(new URL('../schema.sql', import.meta.url), 'utf8')
const UPGRADE = readFileSync(
  new URL('../upgrades/0001_readable_timestamps.sql', import.meta.url), 'utf8')

const TS = 1_755_230_000 // 2025-08-15T03:53:20Z
const HOUR_BUCKET = Math.floor(TS / 3600)

// The store owns its connection, so reading raw rows back needs a second one
// on the same file — ':memory:' is private per connection.
const dir = mkdtempSync(join(tmpdir(), 'sigiltty-relay-test-'))
afterAll(() => rmSync(dir, { recursive: true, force: true }))

let seq = 0
function dbPath(): string {
  return join(dir, `${seq++}.sqlite3`)
}

describe('isoFromUnix', () => {
  test('renders unix seconds as second-precision UTC', () => {
    expect(isoFromUnix(TS)).toBe('2025-08-15T03:53:20Z')
    expect(isoFromUnix(0)).toBe('1970-01-01T00:00:00Z')
  })
})

describe('isoAtOffset', () => {
  // Same instant and the same offsets as watcher/src/timefmt.rs's tests: a
  // watcher line and a relay line for one event must differ only in the
  // milliseconds the relay adds, so the two renderings are pinned in both
  // repositories to the same constants.
  test('collapses a zero offset to Z, matching the log format on Workers', () => {
    expect(isoAtOffset(TS * 1000, 0)).toBe('2025-08-15T03:53:20.000Z')
    expect(isoAtOffset(0, 0)).toBe('1970-01-01T00:00:00.000Z')
    // Whatever else changed, the deployed Worker keeps printing exactly what
    // `new Date().toISOString()` did — its timezone is UTC and cannot be set.
    expect(isoAtOffset(TS * 1000 + 456, 0)).toBe(new Date(TS * 1000 + 456).toISOString())
  })

  test('writes a non-zero offset into the line', () => {
    expect(isoAtOffset(TS * 1000, 8 * 60)).toBe('2025-08-15T11:53:20.000+08:00')
    // Half- and quarter-hour zones exist (India, Nepal) — the offset is
    // minutes, not hours.
    expect(isoAtOffset(TS * 1000, 5 * 60 + 30)).toBe('2025-08-15T09:23:20.000+05:30')
    expect(isoAtOffset(TS * 1000, 5 * 60 + 45)).toBe('2025-08-15T09:38:20.000+05:45')
    // West of Greenwich the local date can be the previous day.
    expect(isoAtOffset(TS * 1000, -7 * 60)).toBe('2025-08-14T20:53:20.000-07:00')
    // Negative offset before the epoch — the case a truncating division of
    // the day would land a day out.
    expect(isoAtOffset(0, -60)).toBe('1969-12-31T23:00:00.000-01:00')
  })

  test('agrees with the platform on every offset the world uses', () => {
    // Cross-check against toISOString for the whole range of real offsets
    // (-12:00 to +14:00, quarter-hour steps): shifting the instant and
    // reading its UTC fields must equal shifting the string.
    for (let minutes = -12 * 60; minutes <= 14 * 60; minutes += 15) {
      const shifted = new Date(TS * 1000 + minutes * 60_000).toISOString()
      expect(isoAtOffset(TS * 1000, minutes).slice(0, 23)).toBe(shifted.slice(0, 23))
    }
  })
})

describe('schema.sql readable columns', () => {
  test('mirror what the store wrote, in the same format as the log', async () => {
    const path = dbPath()
    const store = new SqliteStore(path, SCHEMA)
    await store.createRegistration({
      routingID: 'r1',
      secretHash: 'h',
      platform: 'ios',
      tokenKind: 'alert',
      apnsEnvironment: 'sandbox',
      apnsToken: 'a'.repeat(64),
      createdAt: TS,
      lastSeenAt: TS + 60,
    })
    await store.markSent('r1', 'herdr-server-w1:p4', TS)
    await store.bumpHourly('r1', HOUR_BUCKET)

    const db = new DatabaseSync(path)
    expect(db.prepare('SELECT * FROM registrations').get()).toMatchObject({
      created_at: TS,
      created_at_utc: isoFromUnix(TS),
      last_seen_at: TS + 60,
      last_seen_at_utc: isoFromUnix(TS + 60),
    })
    expect(db.prepare('SELECT * FROM send_log').get()).toMatchObject({
      sent_at: TS,
      sent_at_utc: isoFromUnix(TS),
    })
    // The bucket key is hours since the epoch; readable form is the instant
    // it opened, so 03:53:20 lands in the 03:00:00 bucket.
    expect(db.prepare('SELECT * FROM hourly_counts').get()).toMatchObject({
      hour_bucket: HOUR_BUCKET,
      hour_start_utc: '2025-08-15T03:00:00Z',
    })
    db.close()
  })
})

describe('upgrades/0001_readable_timestamps.sql', () => {
  test('brings a pre-upgrade database to the same rendering', () => {
    // The tables as they were before the readable columns existed — the
    // shape any already-deployed relay still has, since CREATE TABLE IF NOT
    // EXISTS leaves it untouched.
    const db = new DatabaseSync(dbPath())
    db.exec(`
      CREATE TABLE registrations (
        routing_id TEXT PRIMARY KEY, secret_hash TEXT NOT NULL,
        platform TEXT NOT NULL, token_kind TEXT NOT NULL,
        apns_environment TEXT NOT NULL, apns_token TEXT NOT NULL,
        created_at INTEGER NOT NULL, last_seen_at INTEGER NOT NULL);
      CREATE TABLE send_log (
        routing_id TEXT NOT NULL, collapse_key TEXT NOT NULL,
        sent_at INTEGER NOT NULL, PRIMARY KEY (routing_id, collapse_key));
      CREATE TABLE hourly_counts (
        routing_id TEXT NOT NULL, hour_bucket INTEGER NOT NULL,
        count INTEGER NOT NULL, PRIMARY KEY (routing_id, hour_bucket));
    `)
    db.exec(`INSERT INTO registrations VALUES ('r1','h','ios','alert','sandbox','a',${TS},${TS});
             INSERT INTO send_log VALUES ('r1','herdr-server-w1:p4',${TS});
             INSERT INTO hourly_counts VALUES ('r1',${HOUR_BUCKET},1);`)

    db.exec(UPGRADE)

    // Existing rows read back readable — a virtual column is computed on
    // read, so the migration needs no backfill.
    expect(db.prepare('SELECT * FROM registrations').get()).toMatchObject({
      created_at: TS,
      created_at_utc: isoFromUnix(TS),
      last_seen_at_utc: isoFromUnix(TS),
    })
    expect(db.prepare('SELECT * FROM send_log').get())
      .toMatchObject({ sent_at_utc: isoFromUnix(TS) })
    expect(db.prepare('SELECT * FROM hourly_counts').get())
      .toMatchObject({ hour_start_utc: '2025-08-15T03:00:00Z' })

    // Applying it twice is the operator's likely mistake; it must fail loudly
    // rather than half-apply.
    expect(() => db.exec(UPGRADE)).toThrow(/duplicate column name/)
    db.close()
  })
})
