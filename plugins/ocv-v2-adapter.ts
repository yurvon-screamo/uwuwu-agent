// ocv-v2-adapter.ts — V2 adapter for psinetron-opencode-visualizer (V1 plugin).
//
// The upstream plugin targets the V1 plugin API and fails to load on OpenCode V2
// ("Plugin must export a default definition with an id and an effect or setup
// function"). Its default export is a plain V1 factory that only uses
// `directory` from its context, so we can call it directly and translate
// between API generations:
//
//   V2 ctx.tool.hook("execute.before"/"execute.after")  ->  V1 tool hooks
//   V2 ctx.event.subscribe (session.execution.*)        ->  V1 event names
//                                                          the pixel-office
//                                                          frontend understands
//
// The V1 factory starts the in-process Bun.serve visualizer (127.0.0.1:5173)
// and registers this process as a character via WebSocket.

import { Plugin } from "@opencode/plugin"
import V1Visualizer from "psinetron-opencode-visualizer/server"

type V1Hooks = {
  event?: (input: { event: { type: string; properties?: unknown } }) => Promise<void> | void
  "tool.execute.before"?: (input: { tool: string }, output?: unknown) => Promise<void> | void
  "tool.execute.after"?: (input: { tool: string }, output?: unknown) => Promise<void> | void
}

// V2 event type -> V1 event name the frontend reacts to.
const EVENT_MAP: Record<string, string> = {
  "session.execution.started": "session.status",
  "session.execution.succeeded": "session.idle",
  "session.execution.failed": "session.error",
  "session.execution.interrupted": "session.idle",
}

export default Plugin.define({
  id: "ocv-v2-adapter",
  async setup(ctx) {
    const directory = ctx.location.directory

    // The V1 factory only reads `directory`; other context fields are unused.
    const hooks = (await V1Visualizer({ directory } as never)) as V1Hooks

    const registrations: { dispose(): Promise<void> }[] = []

    if (hooks["tool.execute.before"]) {
      registrations.push(
        await ctx.tool.hook("execute.before", (event) => {
          void hooks["tool.execute.before"]!({ tool: event.tool })
        }),
      )
    }
    if (hooks["tool.execute.after"]) {
      registrations.push(
        await ctx.tool.hook("execute.after", (event) => {
          void hooks["tool.execute.after"]!({ tool: event.tool })
        }),
      )
    }

    const controller = new AbortController()
    if (hooks.event) {
      void (async () => {
        try {
          for await (const event of ctx.event.subscribe({ signal: controller.signal })) {
            const type = (event as { type?: string }).type ?? ""
            const mapped = EVENT_MAP[type]
            if (mapped) await hooks.event!({ event: { type: mapped, properties: event } })
          }
        } catch {
          // stream aborted on unload — expected
        }
      })()
    }

    return () => {
      controller.abort()
      for (const registration of registrations) void registration.dispose()
    }
  },
})
