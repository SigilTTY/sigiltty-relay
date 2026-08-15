// Protocol v1 types — mirror docs/PROTOCOL.md §3 exactly. A change here is
// a protocol change and lands in relay, watcher and the SigilTTY app in one
// coordinated release.

export type ApnsEnvironment = 'production' | 'sandbox'

export interface RegisterRequest {
  platform: 'ios'
  tokenKind: 'alert'
  apnsEnvironment: ApnsEnvironment
  apnsToken: string
}

export interface RegisterResponse {
  routingID: string
  secret: string
}

export interface RenewRequest {
  routingID: string
  secret: string
  apnsToken?: string
}

export interface EventEntry {
  routingID: string
  secret: string
  collapseKey: string
  ts: number
  ciphertext: string
}

export type EventResult =
  | 'sent'
  | 'rateLimited'
  | 'unknownRouting'
  | 'badSecret'
  | 'invalid'
  | 'sendFailed'
