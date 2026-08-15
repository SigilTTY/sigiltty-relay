// One log-line shape for both entries. Every line opens with an ISO-8601
// instant, because none of the defaults supply a usable one: `wrangler tail`
// prints its own timestamp in pretty mode only (and in the viewer's
// timezone), `--format json` gives epoch milliseconds, and the self-hosted
// process writes to stdout with no time at all.
//
// UTC, not local: a Worker isolate has no meaningful local timezone, and an
// operator correlating a relay line against an APNs response or a watcher log
// needs one unambiguous reading. The trailing Z says which one it is.
//
// Zero-knowledge (CLAUDE.md): these helpers timestamp lines, they do not
// widen them. Nothing user-derived — APNs token, collapse key, ciphertext —
// may be passed in. Status codes, reasons and counts only.

function line(level: string, message: string): string {
  // Millisecond precision: two sends inside one second are ordinary, and the
  // order between them is usually the thing being debugged.
  return `${new Date().toISOString()} ${level.padEnd(5)} ${message}`
}

export function info(message: string): void {
  console.log(line('INFO', message))
}

export function warn(message: string): void {
  console.warn(line('WARN', message))
}

export function error(message: string): void {
  console.error(line('ERROR', message))
}

/// Renders a stored unix-second timestamp exactly as schema.sql's generated
/// `*_utc` columns do — second precision, explicit Z. The two renderings are
/// deliberately identical so a log line and a database row can be compared by
/// eye; test/log.test.ts pins them against each other so they stay that way.
export function isoFromUnix(seconds: number): string {
  return `${new Date(seconds * 1000).toISOString().slice(0, 19)}Z`
}
