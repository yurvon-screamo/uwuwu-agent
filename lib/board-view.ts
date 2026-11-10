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
    }
  }[]
}

/** Payload одной страницы render (структурная часть ответа ядра). */
export interface BoardViewPage {
  view?: { columns?: ViewColumn[]; rows?: RenderRow[]; rowCount?: number }
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
      view.columns = data.view?.columns ?? []
      rowCount = data.view?.rowCount ?? 0
    }
    const items = data.view?.rows ?? []
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
