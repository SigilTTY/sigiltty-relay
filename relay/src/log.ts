// One log-line shape for both entries. Every line opens with an ISO-8601
// instant carrying an explicit UTC offset, because none of the defaults
// supply a usable one: `wrangler tail` prints its own timestamp in pretty
// mode only (and in the viewer's timezone), `--format json` gives epoch
// milliseconds, and the self-hosted process writes to stdout with no time.
//
// The offset is the host's own, so a self-hosted relay reads in the timezone
// its operator set the box to. On Workers that is always UTC — the runtime
// pins it regardless of the deploying machine's TZ — and a zero offset
// renders as `Z`, so the deployed relay is byte-identical to what it printed
// before this was parameterised. Writing the offset into the line is what
// makes the two safe to mix: a watcher line at +08:00 and a relay line at Z
// need one subtraction, and the line says how much.
//
// Zero-knowledge (CLAUDE.md): these helpers timestamp lines, they do not
// widen them. Nothing user-derived — APNs token, collapse key, ciphertext —
// may be passed in. Status codes, reasons and counts only.

function pad(value: number, width = 2): string {
  return String(value).padStart(width, '0')
}

function offsetSuffix(minutes: number): string {
  if (minutes === 0) return 'Z'
  const abs = Math.abs(minutes)
  // Half- and quarter-hour zones exist (India, Nepal), so this is minutes.
  return `${minutes < 0 ? '-' : '+'}${pad(Math.floor(abs / 60))}:${pad(abs % 60)}`
}

/// Renders an instant at an explicit UTC offset, milliseconds included — two
/// sends inside one second are ordinary, and the order between them is
/// usually the thing being debugged. Pure: the offset is a parameter rather
/// than an ambient lookup, so the formatting is testable without depending on
/// the runner's timezone.
export function isoAtOffset(epochMillis: number, offsetMinutes: number): string {
  // Shift the instant, then read its UTC fields — that IS the local clock at
  // this offset, and it never asks Date for anything zone-dependent.
  const d = new Date(epochMillis + offsetMinutes * 60_000)
  return `${pad(d.getUTCFullYear(), 4)}-${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())}`
    + `T${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())}:${pad(d.getUTCSeconds())}`
    + `.${pad(d.getUTCMilliseconds(), 3)}${offsetSuffix(offsetMinutes)}`
}

function line(level: string, message: string): string {
  const now = new Date()
  // getTimezoneOffset is minutes to ADD to local to reach UTC — the sign is
  // the opposite of the one printed, hence the negation.
  return `${isoAtOffset(now.getTime(), -now.getTimezoneOffset())} ${level.padEnd(5)} ${message}`
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
/// `*_utc` columns do — UTC, second precision, explicit Z. Those columns are
/// UTC and cannot be anything else: D1 runs on Cloudflare, whose clock is
/// UTC, and SQLite rejects `'localtime'` inside a generated column outright
/// ("non-deterministic use of strftime"). test/timestamps.test.ts pins this
/// against the columns so the two renderings stay identical.
export function isoFromUnix(seconds: number): string {
  return `${new Date(seconds * 1000).toISOString().slice(0, 19)}Z`
}
