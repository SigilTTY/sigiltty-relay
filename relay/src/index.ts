// Cloudflare Workers entry: D1 store, platform fetch (HTTP/2 to APNs).
// App and APNs sender are memoized at module level — isolate lifetime —
// so ES256 provider tokens get the 20–60 min reuse APNs requires.

import { createApnsSender } from './apns.ts'
import { createRelay } from './app.ts'
import { D1Store } from './store/d1.ts'

export interface Env {
  DB: D1Database
  APNS_TEAM_ID: string
  APNS_KEY_ID: string
  APNS_TOPIC: string
  // Secret (`wrangler secret put APNS_PRIVATE_KEY`): the .p8 file contents.
  APNS_PRIVATE_KEY: string
}

let app: ReturnType<typeof createRelay> | undefined

export default {
  async fetch(request, env, ctx): Promise<Response> {
    app ??= createRelay({
      store: new D1Store(env.DB),
      apns: createApnsSender({
        teamID: env.APNS_TEAM_ID,
        keyID: env.APNS_KEY_ID,
        topic: env.APNS_TOPIC,
        privateKeyPEM: env.APNS_PRIVATE_KEY,
      }),
    })
    return app.fetch(request, env, ctx)
  },

  async scheduled(_controller, env): Promise<void> {
    await new D1Store(env.DB).purge(Math.floor(Date.now() / 1000))
  },
} satisfies ExportedHandler<Env>
