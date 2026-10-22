// Контракт sticky-retry → UI: типы, RPC-определение и чистые форматтеры.
// Общий модуль серверного плагина (index.ts) и TUI-плагина (tui.tsx):
// сервер эмитит RetryStatusEvent через RPC-порт "sticky-retry", TUI рендерит.
// Все строки, уходящие в UI, проходят scrubDetail — секреты не покидают хост.

export type RetryEventKind =
  | "retry" // форвардер: очередная попытка после retryable-отказа
  | "recovered" // форвардер: успех после ≥1 ретрая этого запроса
  | "give-up" // форвардер: отказ от цикла (stale-stream / non-retryable в SSE)
  | "fallback-retry" // session.hook("retry"): трафик мимо форвардера
  | "continue-scheduled" // auto-continue: синтетический репромплан запланирован
  | "continue-sent" // auto-continue: репромплан отправлен
  | "continue-failed" // auto-continue: отправка не удалась
  | "continue-cancelled" // auto-continue: отменён (интерапт юзера)
  | "continue-capped"; // auto-continue: лимит подряд исчерпан

export interface RetryStatusEvent {
  readonly kind: RetryEventKind;
  /** Сессия-владелец, когда известна (заголовок model.request / событие). */
  readonly sessionID?: string;
  /** display-URL апстрима: origin+path, без query. */
  readonly url?: string;
  /** HTTP-статус апстрима, если был. */
  readonly status?: number;
  /** Причина транспортной ошибки или тип ошибки сессии (scrub). */
  readonly transport?: string;
  readonly attempt?: number;
  /** Задержка до следующей попытки, мс. */
  readonly delayMs?: number;
  /** Тело ошибки провайдера, scrubbed, ≤400 символов. */
  readonly detail?: string;
}

export const RETRY_RPC = {
  id: "sticky-retry",
  methods: {},
  events: {
    status: {
      schema: {
        type: "object",
        properties: {
          kind: {
            type: "string",
            enum: [
              "retry",
              "recovered",
              "give-up",
              "fallback-retry",
              "continue-scheduled",
              "continue-sent",
              "continue-failed",
              "continue-cancelled",
              "continue-capped",
            ],
          },
          sessionID: { type: "string" },
          url: { type: "string" },
          status: { type: "number" },
          transport: { type: "string" },
          attempt: { type: "number" },
          delayMs: { type: "number" },
          detail: { type: "string" },
        },
        required: ["kind"],
        additionalProperties: false,
      },
    },
  },
} as const;

export const DETAIL_MAX_CHARS = 400;

/** Маскирование секретов в тексте для UI/лога: URL, авторизация, ключи.
 *  Тела ошибок шлюзов иногда эхом возвращают заголовки запроса. */
export const scrubDetail = (text: string): string =>
  text
    .replace(/https?:\/\/\S+/g, "[url]")
    .replace(/\b(Basic|Bearer)\s+[A-Za-z0-9._~+/=-]+/gi, "$1 [redacted]")
    .replace(/\bAIza[A-Za-z0-9_-]{10,}\b/g, "[redacted]")
    .replace(/\bsk-[A-Za-z0-9]{8,}\b/g, "[redacted]")
    .replace(
      /\b(x-api-key|api-key|apikey|authorization|cookie)\b\s*[:=]\s*\S+/gi,
      "$1: [redacted]",
    )
    .replace(
      /("(?:apiKey|api_key|authorization|token|access_token|refresh_token)"\s*:\s*")[^"]+/gi,
      "$1[redacted]",
    )
    .slice(0, DETAIL_MAX_CHARS);

/** Вытащить человекочитаемое сообщение из тела ошибки провайдера.
 *  Три формы: OpenAI {error:{message,code}}, Anthropic
 *  {type:"error",error:{type,message}}, Gemini {error:{code,message,status}}.
 *  Fallback — плоский текст. Результат всегда scrubbed. */
export const extractErrorDetail = (bodyText: string): string => {
  const trimmed = bodyText.trim();
  if (trimmed === "") return "";
  try {
    const parsed = JSON.parse(trimmed) as {
      error?: { message?: unknown; code?: unknown; status?: unknown; type?: unknown };
      message?: unknown;
    };
    const fromError = (e: {
      message?: unknown;
      code?: unknown;
      status?: unknown;
      type?: unknown;
    }): string => {
      const message = typeof e.message === "string" ? e.message : "";
      if (message === "") return "";
      const qualifier =
        typeof e.code === "string" || typeof e.code === "number"
          ? String(e.code)
          : typeof e.status === "string"
            ? e.status
            : undefined;
      return qualifier === undefined ? message : `${qualifier}: ${message}`;
    };
    if (parsed.error !== undefined) {
      const detail = fromError(parsed.error);
      if (detail !== "") return scrubDetail(detail);
    }
    if (typeof parsed.message === "string" && parsed.message !== "") {
      return scrubDetail(parsed.message);
    }
  } catch {
    // не JSON — плоский текст ниже
  }
  return scrubDetail(trimmed);
};

const fmtDelay = (ms: number): string =>
  ms % 60_000 === 0 ? `${Math.round(ms / 60_000)}m` : `${Math.max(1, Math.round(ms / 1000))}s`;

/** Строка для футера/тоста: что случилось, какая попытка, когда следующая. */
export const formatRetryLine = (event: RetryStatusEvent): string => {
  const attempt = event.attempt !== undefined ? ` attempt ${event.attempt}` : "";
  const next = event.delayMs !== undefined ? ` · next in ${fmtDelay(event.delayMs)}` : "";
  const cause = event.transport ?? event.detail ?? "";
  const causeText = cause === "" ? "" : ` · ${cause.split("\n")[0]}`;
  switch (event.kind) {
    case "retry":
      return `⟳ HTTP ${event.status ?? "?"}${attempt}${next}${causeText}`;
    case "fallback-retry":
      return `⟳ ${event.transport ?? "error"}${attempt}${next}${causeText}`;
    case "recovered":
      return `✓ recovered${attempt}${causeText}`;
    case "give-up":
      return `✗ gave up${attempt}${causeText}`;
    case "continue-scheduled":
      return `⟳ auto-continue${attempt}${next}${causeText}`;
    case "continue-sent":
      return `→ auto-continue sent${attempt}`;
    case "continue-failed":
      return `✗ auto-continue failed${attempt}${causeText}`;
    case "continue-cancelled":
      return "· auto-continue cancelled";
    case "continue-capped":
      return `✗ auto-continue cap reached${attempt}`;
  }
};

export type ToastVariant = "info" | "success" | "warning" | "error";

export const toastVariant = (kind: RetryEventKind): ToastVariant => {
  switch (kind) {
    case "retry":
    case "fallback-retry":
      return "warning";
    case "recovered":
    case "continue-sent":
      return "success";
    case "give-up":
    case "continue-capped":
    case "continue-failed":
      return "error";
    case "continue-scheduled":
    case "continue-cancelled":
      return "info";
  }
};

/** Наибольшая пауза цикла — Retry-After-cap; футер живёт не меньше неё. */
export const RETRY_AFTER_CAP_MS = 30 * 60_000;
export const FOOTER_FRESH_MS = RETRY_AFTER_CAP_MS + 5 * 60_000;
