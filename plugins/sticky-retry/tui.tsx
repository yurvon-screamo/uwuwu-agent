// TUI-компаньон sticky-retry: подписка на RPC-события серверного плагина
// (порт "sticky-retry", событие "status") и показ ретраев в интерфейсе —
// постоянная строка в футере промпта для ТЕКУЩЕЙ сессии + тосты для всех.
import { Plugin } from "@opencode/plugin/tui"
import { Show } from "solid-js"
import {
  FOOTER_FRESH_MS,
  RETRY_RPC,
  type RetryStatusEvent,
  formatRetryLine,
  toastVariant,
} from "./contract"

type Entry = RetryStatusEvent & { readonly at: number }

interface RetryStore {
  readonly bySession: Record<string, Entry>
}

/** Kinds representing an active retry loop — footer-worthy. */
const FOOTER_KINDS = new Set<RetryStatusEvent["kind"]>(["retry", "fallback-retry"])
/** Kinds closing the loop: the footer line disappears right away. */
const CLOSING_KINDS = new Set<RetryStatusEvent["kind"]>(["recovered", "give-up"])

export default Plugin.define({
  id: "sticky-retry.ui",
  setup(context) {
    const [store, updateStore] = context.storage.memory<RetryStore>("status", {
      initial: { bySession: {} },
    })
    let lastToastKey = ""

    const handleStatusEvent = (event: RetryStatusEvent): void => {
      const entry: Entry = { ...event, at: Date.now() }
      if (event.sessionID !== undefined && FOOTER_KINDS.has(event.kind)) {
        void updateStore(draft => {
          draft.bySession[event.sessionID as string] = entry
        })
      }
      if (event.sessionID !== undefined && CLOSING_KINDS.has(event.kind)) {
        void updateStore(draft => {
          delete draft.bySession[event.sessionID as string]
        })
      }

      // Toasts for every event, deduped per (session, kind, attempt) —
      // a retry loop is silent between attempts, toasts only on change.
      const current = context.ui.router.current()
      const currentSessionID = current.type === "session" ? current.sessionID : undefined
      const dedupeKey = `${event.sessionID ?? ""}|${event.kind}|${event.attempt ?? 0}`
      if (dedupeKey === lastToastKey) return
      lastToastKey = dedupeKey
      const foreign =
        event.sessionID !== undefined && event.sessionID !== currentSessionID
          ? `[…${event.sessionID.slice(-6)}] `
          : ""
      context.ui.toast.show({
        title: "sticky-retry",
        message: `${foreign}${formatRetryLine(event)}`.trim(),
        variant: toastVariant(event.kind),
      })
    }

    const unsubscribe = context.client
      .rpc(RETRY_RPC)
      .events.on("status", raw => handleStatusEvent(raw.data as unknown as RetryStatusEvent))

    // Без свипера запись пережила бы худшую паузу цикла: fresh-окно =
    // Retry-After-cap + запас (FOOTER_FRESH_MS), проверка раз в минуту.
    const sweeper = setInterval(() => {
      const cutoff = Date.now() - FOOTER_FRESH_MS
      void updateStore(draft => {
        for (const [sessionID, entry] of Object.entries(draft.bySession)) {
          if (entry.at < cutoff) delete draft.bySession[sessionID]
        }
      })
    }, 60_000)

    context.ui.slot({
      append: "prompt.footer.status",
      render: input => <RetryFooter sessionID={input.sessionID} store={store} />,
    })

    // Verification command: renders a synthetic retry→recovered pair without
    // waiting for a real provider outage.
    context.keymap.layer(() => ({
      mode: "global",
      commands: [
        {
          id: "sticky-retry.test",
          title: "Simulate retry status",
          description: "sticky-retry: synthetic retry + recovery in the current session",
          group: "sticky-retry",
          palette: true,
          slash: { name: "sticky-retry-test" },
          run: () => {
            const current = context.ui.router.current()
            const sessionID = current.type === "session" ? current.sessionID : undefined
            handleStatusEvent({
              kind: "retry",
              sessionID,
              status: 429,
              attempt: 1,
              delayMs: 420_000,
              detail: "simulated: rate limited",
            })
            setTimeout(() => {
              handleStatusEvent({
                kind: "recovered",
                sessionID,
                status: 200,
                attempt: 2,
              })
            }, 3_000)
          },
        },
      ],
    }))

    return () => {
      clearInterval(sweeper)
      unsubscribe()
    }
  },
})

// Доступ к стору — внутри when-выражения: prop-геттер Show переоценивается в
// реактивном memo, трекинг Solid работает; компонент сверху не трекается.
const RetryFooter = (props: { sessionID?: string; store: RetryStore }) => {
  const line = () => {
    if (props.sessionID === undefined) return undefined
    const entry = props.store.bySession[props.sessionID]
    if (entry === undefined || Date.now() - entry.at >= FOOTER_FRESH_MS) return undefined
    return formatRetryLine(entry)
  }
  return (
    <Show when={line()}>
      {text => <text>{text()}</text>}
    </Show>
  )
}
