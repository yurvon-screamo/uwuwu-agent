// lib/board-view.ts — полная постраничная выборка доски (database render).
// Серверный pageSize render — 50: один вызов отдаёт только первую страницу,
// остальные строки тихо невидимы (прод-кейс 05.11: доска 145 строк, адаптер
// видел 50 — фабрика чистила живой трекинг «строка исчезла»). Прецедент и
// контракт page — CLI collect_board_view (cli/src/siyuan_task.rs): терминация
// по валидному rowCount или пустой странице, дедуп по id, кап страниц с
// громкой ошибкой вместо тихой усечки.

export interface ViewColumn {
  id?: string
  name?: string
}

export interface RenderRow {
  id?: string
  cells?: {
    value?: {
      keyID?: string
      type?: string
      block?: { id?: string; content?: string; refSubtype?: string }
      /** render живого ядра несёт isDetached у detached-строк (fixtures) */
      isDetached?: boolean
      mSelect?: { content?: string }[]
      text?: { content?: string }
      /** ключ типа url (PR-колонка, паритет Rust /url/content) и родня */
      url?: { content?: string }
      email?: { content?: string }
      phone?: { content?: string }
      /** date-клетка: content — epoch ms (истина), formattedContent —
       *  дисплей ядра (локаль ядра, перекрывает записанную строку). */
      date?: { content?: number; formattedContent?: string }
      /** checkbox-клетка (колонка «Autodone», автодон) */
      checkbox?: { checked?: boolean }
    }
  }[]
}

/** Payload одной страницы render. Две формы ядра: table — `view.rows` +
 *  `rowCount`; kanban — `view.groups[].cards` + `cardCount` (форма значений
 *  ячеек та же; паритет CLI parse_board_page, siyuan_task.rs). */
export interface BoardViewPage {
  view?: {
    columns?: ViewColumn[]
    /** kanban-вид: колонки в `fields` (паритет CLI parse_board_page) */
    fields?: ViewColumn[]
    rows?: RenderRow[]
    rowCount?: number
    groups?: { cards?: RenderRow[] }[]
    cardCount?: number
  }
}

/** Строки страницы в форме, независимой от вида: table rows, иначе kanban
 *  cards (карточка несёт значения в `values` — нормализуем в `cells`,
 *  форма value-ячеек та же). Счётчик — rowCount ?? cardCount. */
export function boardPageItems(
  view: BoardViewPage["view"],
): { items: RenderRow[]; count: number } {
  const rows = view?.rows ?? []
  if (rows.length > 0) return { items: rows, count: view?.rowCount ?? 0 }
  const cards = (view?.groups ?? []).flatMap((g) => g.cards ?? [])
  const items = cards.map((card) => {
    const withValues = card as RenderRow & { values?: RenderRow["cells"] }
    return { ...withValues, cells: withValues.cells ?? withValues.values }
  })
  return { items, count: view?.cardCount ?? 0 }
}

export type FetchBoardPage = (page: number) => Promise<BoardViewPage>

export interface BoardView {
  columns: ViewColumn[]
  rows: RenderRow[]
}

/** Страховка от зависания на битом rowCount (100×50 = 5000 строк — запас ×34
 *  от текущих досок); достижение капа — громкая ошибка, не тихая усечка. */
export const MAX_RENDER_PAGES = 100

/** Полная выборка доски по страницам: терминация по валидному rowCount
 *  (валиден и > 0; при конкурентных правках счётчик может устареть вниз —
 *  тогда терминирует пустая страница) или по пустой странице; дубли строк
 *  на стыке страниц (дрейф при конкурентных правках) дедупятся по id —
 *  пустые id не дедупятся. */
export async function collectBoardView(fetchPage: FetchBoardPage): Promise<BoardView> {
  const view: BoardView = { columns: [], rows: [] }
  const seenIds = new Set<string>()
  let rowCount = 0
  for (let page = 1; page <= MAX_RENDER_PAGES; page++) {
    const data = await fetchPage(page)
    if (page === 1) {
      // kanban-вид несёт колонки в `fields`, table — в `columns` (паритет CLI)
      view.columns = data.view?.columns ?? data.view?.fields ?? []
      rowCount = data.view?.rowCount ?? data.view?.cardCount ?? 0
    }
    const { items } = boardPageItems(data.view)
    if (items.length === 0) return view
    for (const row of items) {
      const id = row.id ?? ""
      if (id !== "" && seenIds.has(id)) continue
      if (id !== "") seenIds.add(id)
      view.rows.push(row)
    }
    if (rowCount > 0 && view.rows.length >= rowCount) return view
  }
  throw new Error(
    `read_database_cells: превышен кап ${MAX_RENDER_PAGES} страниц render — доска усечена: ` +
      `собрано ${view.rows.length} строк, rowCount сервера ${rowCount}`,
  )
}
