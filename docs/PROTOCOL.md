# SigilTTY Relay Protocol v1

**Normative single source** for the offline-push pipeline (SigilTTY `docs/design/herdr-offline-push.md`, ADR-0014). Three implementations consume this document — the SigilTTY app (registration, config writing, NSE decryption), the Workers relay, and the Rust watcher. A breaking change bumps the `/v1/` path and the payload `v` field, and lands in all three in one coordinated release; nothing here may drift unilaterally.

## 1. Parties and trust model

| Party | Runs on | Knows |
|---|---|---|
| **App** | user's iPhone | everything (it is the user) |
| **Watcher** | user's server | everything on that server: targets, agent states, device public keys |
| **Relay** | developer's Cloudflare account | routing IDs, APNs tokens, collapse keys, timing — **never content** |

The relay is honest-but-curious: event content is end-to-end encrypted from watcher to device. The zero-knowledge promise is structural — **no protocol revision may add a plaintext field that carries user content** (server names, agent names, statuses). What the relay unavoidably learns and we accept: which routing ID gets events, how often, and an opaque per-pane collapse key (correlation metadata needed for rate limiting and APNs collapse).

Registration is anonymous: no accounts, no identifiers beyond the APNs token the device already holds. Device ↔ server pairing rides the user's own SSH access — the app writes the config file (§6); the relay is never part of pairing.

## 2. Cryptography

- **HPKE** (RFC 9180), base mode, single-shot seal:
  - KEM: DHKEM(X25519, HKDF-SHA256)
  - KDF: HKDF-SHA256
  - AEAD: ChaCha20-Poly1305

  This is CryptoKit's `HPKE.Ciphersuite(kem: .Curve25519_HKDF_SHA256, kdf: .HKDF_SHA256, aead: .chachaPoly)` and the Rust `hpke` crate's matching parameterization. **Pinned** — the ciphersuite is not negotiable within v1.
- `info` = `"sigiltty-relay/v1/event"` (UTF-8).
- `aad` = the event's `collapseKey` (UTF-8). Binds a ciphertext to its pane key: a relay (or replayer) cannot re-route a ciphertext under a different collapse key without the NSE's open failing. The NSE reads the collapse key back as `request.identifier`.
- Wire format of an encrypted event: `base64(enc ‖ ct)` — the 32-byte encapsulated key concatenated with the AEAD ciphertext, standard Base64.
- Device key pair: X25519. Private key lives in the app's Keychain (access group shared with the NSE, `kSecAttrAccessibleAfterFirstUnlock`). Public key (32 bytes, standard Base64) goes **only** into the server config file — the relay never sees it.

## 3. HTTP API (App/Watcher → Relay)

All requests `Content-Type: application/json` over HTTPS. Malformed body → `400`.

### 3.1 Register (App)

```
POST /v1/register
{ "platform": "ios",
  "tokenKind": "alert",              // "liveActivity" reserved, not accepted in v1
  "apnsEnvironment": "production",   // "sandbox" for development-signed builds
  "apnsToken": "<hex>" }
→ 200 { "routingID": "<uuid>", "secret": "<base64, 32 bytes>" }
```

The relay stores SHA-256(secret), never the secret. Registrations expire on a **sliding 30-day TTL**; expired registrations are deleted.

### 3.2 Renew (App)

```
POST /v1/renew
{ "routingID": "...", "secret": "...", "apnsToken": "<hex, optional>" }
→ 204 | 401 (bad secret) | 404 (unknown/expired routing)
```

Refreshes the sliding TTL and optionally rotates the APNs token. The app calls this on every health check; a `404` means the app must re-register and rewrite server configs.

### 3.3 Events (Watcher)

```
POST /v1/events
{ "events": [ { "routingID": "...", "secret": "...",
                "collapseKey": "...", "ts": <unix seconds>,
                "ciphertext": "<base64>" }, ... ] }   // ≤ 16 entries
→ 200 { "results": [ "sent" | "rateLimited" | "unknownRouting" | "badSecret" | "invalid" | "sendFailed", ... ] }
```

Per-entry validation, in order: secret hash matches; `|now − ts| ≤ 300 s` (replay window); `collapseKey` ≤ 64 ASCII chars; rate limits (§7). Entries fail independently — one bad device never blocks the others. A watcher fans one agent event out as N entries, one per device in its config, each sealed to that device's public key.

`sendFailed` is a transient APNs/network failure: the watcher may retry that entry after backoff. Only `sent` advances the per-pane rate-limit clock, so a retry is never blocked by its own failed attempt.

APNs `410 Unregistered` on delivery → the relay deletes the registration; subsequent events return `unknownRouting`.

## 4. APNs mapping

```
apns-push-type: alert        apns-priority: 10
apns-topic: com.sigiltty.shell
apns-collapse-id: <collapseKey>
{ "aps": { "alert": { "title": "SigilTTY", "body": "An agent needs attention." },
           "sound": "default", "mutable-content": 1 },
  "v": 1, "e": "<ciphertext base64>" }
```

The `aps.alert` text is the **fallback** — shown only if the NSE fails (decryption error, missing key, extension timeout). The NSE decrypts `e` and rewrites title/subtitle/body per §5. APNs host follows the registration's `apnsEnvironment` (`api.push.apple.com` / `api.sandbox.push.apple.com`). The relay sets `apns-expiration = ts + 21600` (6 h): a stale agent alert must not surface a day later; within the window, collapse-id replacement handles supersession.

## 5. Encrypted event payload

Plaintext (JSON, UTF-8) inside the HPKE envelope:

```
{ "v": 1,
  "serverID": "<uuid>",        // CDServer.id — synced, identical across the user's devices
  "serverName": "...",
  "paneID": "w1:p4",
  "herdrSession": "..." | null,
  "agentLabel": "...",         // watcher-computed: agent.name > agent kind > "Agent"
  "paneLabel": "..." | null,   // from the config target's label
  "status": "blocked" | "done",
  "ts": <unix seconds> }
```

NSE rendering mirrors the app's online copy exactly (`HerdrNotifier`): title `"{agentLabel} needs attention"` (blocked) / `"{agentLabel} finished"` (done), subtitle `serverName`, body `paneLabel` if present. The decrypted `serverID`/`paneID` go into `userInfo` for tap routing (focus live session, else locate the Agent Record in the Agents surface).

## 6. Server config file (written by the App over SSH)

`$XDG_CONFIG_HOME/sigiltty/relay.json`, falling back to `~/.config/sigiltty/relay.json`:

```
{ "v": 1,
  "relayURL": "https://…",
  "serverID": "<uuid>", "serverName": "...",
  "herdrBinary": "/abs/path/to/herdr",
  "expiresAt": <unix seconds>,           // watcher TTL — app renews on every connection
  "devices": [ { "routingID": "...", "secret": "...",
                 "publicKey": "<base64 X25519>", "platform": "ios" } ],
  "targets": [ { "paneID": "w1:p4", "herdrSession": "..." | null, "label": "..." } ] }
```

The file is the **source of truth for opt-in**: it exists iff offline push is enabled for this server. Disable = stop watcher + delete file (and the app's local cache flag). The watcher reads it **once at startup**; any change (targets, devices, renewal) is applied by the app rewriting the file and restarting the watcher — the watcher never self-reloads.

## 7. Limits

| Knob | Value |
|---|---|
| Watcher hysteresis window | 10 s stable before a transition counts |
| Rate limit per (routingID, collapseKey) | 1 event / 60 s |
| Rate limit per routingID | 30 events / hour |
| Registration TTL | sliding 30 days |
| Watcher config TTL (`expiresAt`) | 7 days, renewed per app connection |
| Replay window | ± 300 s |
| Events per POST | ≤ 16 |
| `collapseKey` length | ≤ 64 ASCII |

## 8. Collapse key (App-side contract)

```
herdr-<serverID>-<paneID>          e.g. herdr-5E1C…D2A9-w1:p4
```

This exact string is also the app's **local** notification identifier for the same pane, so a remote push replaces a delivered local banner (and vice versa) instead of stacking, and `noteFocused` clears both with one `removeDeliveredNotifications` call. Changing this format is an app-repo change first; the watcher merely echoes what §6 implies.

## 9. Watcher behavior contract

- One concurrent `herdr agent wait` loop per target — level-triggered, departure `--until` set, `--timeout 300000` heartbeat; identical CLI semantics to the app's online Agent Watch (herdr ≥ 0.8.0, absolute path from config).
- **Hysteresis**: after observing a transition, the state must hold for the full window (§7) before it is compared against the last *reported* state; bounces inside the window vanish. Only stable `→blocked` / `→done` are reported. herdr's detector is known to flap — this window is the fix's home; the app does no further filtering.
- Seed semantics mirror the online watch: the first observation per target after startup never reports (the user just had the app open); a target's agent *appearing* later (missing → status) is a real event; missing is silent.
- **Single instance** per server: lock file `$XDG_CONFIG_HOME/sigiltty/watcher.lock` (flock + PID; the app uses the PID for stop/restart).
- Exit conditions: `expiresAt` in the past (checked per re-arm), config file missing/unreadable, persistent herdr failures after bounded backoff. Exiting is always silent — the app's health check (per connection) is the recovery path; the watcher never notifies about itself.

## 10. Bootstrap contract

- Binary: `$XDG_DATA_HOME/sigiltty/sigiltty-watcher` (fallback `~/.local/share/sigiltty/`), version marker alongside as `sigiltty-watcher.version`.
- The app sends a single `sh -c`-wrapped bootstrap over SSH: compare version marker → on mismatch `curl` the target-specific binary from the release CDN, verify **sha256** against the app-known manifest hash, atomically move into place → start under `setsid`, detached.
- CDN layout: `<base>/watcher/<version>/sigiltty-watcher-<target>` + `<base>/watcher/<version>/SHA256SUMS` where `<target>` ∈ `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`. (`<base>` TBD — same Cloudflare account as the relay, R2 or Workers-served.)
- Any bootstrap failure rolls the opt-in switch back in the app and leaves no half-deployed state (partial downloads go to a temp path, never the final name).
