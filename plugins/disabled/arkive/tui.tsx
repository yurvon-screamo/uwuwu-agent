// TUI-компаньон arkive: трим пересоздаёт сессию (remove+import) — кэш
// истории в TUI остаётся пустым (прод-инцидент 2026-10-24). По RPC-событию
// "session-rewritten" инвалидируем кэш и пересинхронизируем сообщения.
// Контракт дублирован в magic-compact arkive-v2/src/plugin.ts (ARKIVE_RPC) —
// держать в синке.
import { Plugin } from "@opencode/plugin/tui"

const ARKIVE_RPC = {
  id: "arkive-v2",
  methods: {},
  events: {
    "session-rewritten": {
      schema: {
        type: "object",
        properties: { sessionID: { type: "string" } },
        required: ["sessionID"],
        additionalProperties: false,
      },
    },
  },
} as const

export default Plugin.define({
  id: "arkive.ui",
  setup(context) {
    const unsubscribe = context.client
      .rpc(ARKIVE_RPC)
      .events.on("session-rewritten", raw => {
        const sessionID = (raw.data as { sessionID?: string }).sessionID
        if (sessionID === undefined) return
        void context.data.session.invalidate(sessionID)
        void context.data.session.message.sync(sessionID)
      })
    return unsubscribe
  },
})
