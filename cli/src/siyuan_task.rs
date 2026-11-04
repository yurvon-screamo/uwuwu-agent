//! SiYuan-бекенд для uwuwu-cli task: те же команды и форматы вывода,
//! что и у афин-реализации (контракт для агентов/AGENTS.md не меняется).
//!
//! Модель (v1):
//! - доска «Задачи <Proj>» = док /<project>/Задачи <project> с database-блоком
//! - строка задачи: Title (primary), Status (select), Branch (text), PR (url)
//! - тело задачи = саб-док дока доски: /<project>/Задачи <project>/<title>
//! - комменты = секция «## Комменты» в теле (строки, реплаи — отступом)

use crate::siyuan::SiYuan;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use std::collections::HashSet;

/// Валидный хит db_search: имя доски совпадает точно, анкор — по hPath
/// (первый сегмент — имя ноутбука) или как OR-страховка по blockID == doc_id.
fn pick_search_hit(results: &[Value], expect_hpath: &str, title: &str, doc_id: &str) -> Option<(String, String)> {
    results.iter().find_map(|hit| {
        let av_name = hit.get("avName").and_then(|n| n.as_str()).unwrap_or("");
        let av_id = hit.get("avID").and_then(|a| a.as_str()).unwrap_or("");
        let block_id = hit.get("blockID").and_then(|b| b.as_str()).unwrap_or("");
        let hpath = hit.get("hPath").and_then(|h| h.as_str()).unwrap_or("");
        let anchored = hpath == expect_hpath || block_id == doc_id;
        (av_name == title && anchored && !av_id.is_empty())
            .then(|| (av_id.to_string(), block_id.to_string()))
    })
}

pub struct SyBoard {
    pub doc_id: String,
    pub av_id: String,
    pub block_id: String,
    pub hpath: String,
    pub notebook: String,
}

impl SyBoard {
    /// Путь тела-саб-дока: единственная точка вклейки тайтла в hpath.
    /// '/' в тайтле заменяется на '∕' (U+2215) — SiYuan трактует '/' как
    /// разделитель сегментов и не создаёт отсутствующих родителей
    /// (createDocWithMd отвечает «document metadata is not ready»).
    pub fn body_hpath(&self, title: &str) -> String {
        format!("{}/{}", self.hpath, path_safe(title))
    }
}

/// Тайтл → безопасный сегмент hpath (конвенция миграции bind_rows.py: '/' → U+2215).
pub(crate) fn path_safe(title: &str) -> String {
    title.replace('/', "\u{2215}")
}

/// Каноническая форма тайтла для сравнения: ∕ ≡ / (регистр не важен).
/// Юзер ищет «RU/EN» с ASCII-слэшем и находит строку с ∕ из миграции/санитайза.
pub(crate) fn normalize_title(title: &str) -> String {
    path_safe(title).to_lowercase()
}

/// Доска проекта: док /<project>/Задачи <project> + database-блок.
pub async fn find_board(sy: &SiYuan, project: &str) -> Result<SyBoard> {
    find_board_titled(sy, project, &format!("Задачи {project}")).await
}

pub async fn find_board_titled(sy: &SiYuan, project: &str, title: &str) -> Result<SyBoard> {
    let nb = sy.ensure_notebook("Projects").await?;
    let hpath = format!("/{p}/{title}", p = project);
    let doc_id = sy
        .doc_ids_by_hpath(&nb, &hpath)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("доска «{title}» не найдена ({hpath})"))?;
    // database-блок внутри дока: av-блок ядра (data-av-id=) или legacy i-носитель (avID=)
    let rows = sy
        .sql(&format!(
            "select id, markdown from blocks where root_id='{doc_id}' \
             and (type='av' or (type='i' and markdown like '%avID=%')) limit 5"
        ))
        .await?;
    let mut av_id = String::new();
    let mut block_id = String::new();
    for r in &rows {
        let md = r.get("markdown").and_then(|m| m.as_str()).unwrap_or("");
        if let Some(av) = extract_av_id(md) {
            av_id = av;
            block_id = r.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
            break;
        }
    }
    if av_id.is_empty() {
        // fallback: MCP db_search по имени; первый хит не доверяем —
        // несвежий индекс отдаёт фаззи-хиты чужих досок
        let v = sy.db_search(title).await?;
        let results = v
            .pointer("/data/results")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();
        let expect_hpath = format!("Projects{hpath}");
        match pick_search_hit(&results, &expect_hpath, title, &doc_id) {
            Some((av, block)) => {
                av_id = av;
                block_id = block;
            }
            None => bail!(
                "database доски «{title}» не найден: поиск вернул {} хит(ов), валидного нет (ожидался hPath «{expect_hpath}»)",
                results.len()
            ),
        }
    }
    Ok(SyBoard { doc_id, av_id, block_id, hpath, notebook: nb })
}

fn extract_av_id(md: &str) -> Option<String> {
    let markers = [("data-av-id=", "data-av-id=".len()), ("avID=", "avID=".len())];
    let (idx, marker_len) = markers
        .iter()
        .filter_map(|(marker, len)| md.find(marker).map(|pos| (pos, *len)))
        .min_by_key(|(pos, _)| *pos)?;
    let rest = md[idx + marker_len..].trim_start();
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let value_end = rest[1..].find(quote)?;
    Some(rest[1..1 + value_end].to_string())
}

#[derive(Clone, Debug)]
pub struct SyTask {
    pub item_id: String,
    pub title: String,
    pub status: String,
    pub branch: String,
    pub pr: String,
    pub body_doc: Option<String>,
}

/// Лимит страниц render при пагинации: страховка от зависания на битом
/// rowCount (100×50 = 5000 строк — запас ×50 от текущих досок); достижение
/// капа — громкая ошибка, не тихая усечка.
const MAX_RENDER_PAGES: u64 = 100;

/// Вид доски после полной постраничной выборки render: колонки + уникальные
/// items всех страниц.
struct BoardView {
    columns: Vec<Value>,
    items: Vec<Value>,
}

/// Полная выборка доски по страницам render: терминация по валидному rowCount
/// или пустой странице, дедуп по id, кап страниц с громкой ошибкой.
async fn collect_board_view(sy: &SiYuan, board: &SyBoard) -> Result<BoardView> {
    let mut view = BoardView { columns: Vec::new(), items: Vec::new() };
    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut row_count: Option<u64> = None;
    let mut page: u64 = 1;
    loop {
        let v = sy.db_render(&board.av_id, &board.block_id, page).await?;
        // structuredContent = {action, data:{...}} — берём data
        let data = v.get("data").unwrap_or(&v);
        if page == 1 {
            view.columns = data
                .pointer("/view/columns")
                .and_then(|c| c.as_array())
                .cloned()
                .unwrap_or_default();
            row_count = data.pointer("/view/rowCount").and_then(|r| r.as_u64());
        }
        let items = data
            .pointer("/view/rows")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();
        if items.is_empty() {
            // терминация всегда: rowCount невалиден (0/отсутствует) или устарел
            // вниз при конкурентных удалениях — счётчик не используется
            break;
        }
        for it in items {
            // дедуп по id: дрейф страниц при конкурентных правках доски даёт
            // дубль строки на стыке, а тот сломал бы resolve id-first
            // («неоднозначный id»); пустые id не дедупятся
            let id = it.get("id").and_then(|i| i.as_str()).unwrap_or("");
            if !id.is_empty() && !seen_ids.insert(id.to_string()) {
                continue;
            }
            view.items.push(it);
        }
        // счётчик терминации — уникальные строки (после дедупа, до фильтра тайтлов)
        if row_count.is_some_and(|rc| rc > 0 && view.items.len() as u64 >= rc) {
            break;
        }
        page += 1;
        if page > MAX_RENDER_PAGES {
            bail!(
                "read_rows: превышен кап {MAX_RENDER_PAGES} страниц render — доска усечена: собрано {} строк, rowCount сервера {row_count:?}",
                view.items.len()
            );
        }
    }
    Ok(view)
}

/// Строки доски через MCP database render (полная выборка, постранично).
pub async fn read_rows(sy: &SiYuan, board: &SyBoard) -> Result<Vec<SyTask>> {
    let view = collect_board_view(sy, board).await?;
    let mut out = Vec::new();
    for it in view.items {
        if let Some(t) = parse_render_row(&it, &view.columns) {
            out.push(t);
        }
    }
    Ok(out)
}

/// Строка render → SyTask. Title из primary-клетки; body_doc — docId из
/// value.block.id той же клетки (привязка строки к документу-телу).
fn parse_render_row(it: &Value, keys: &[Value]) -> Option<SyTask> {
    let key_name = |id: &str| -> String {
        keys.iter()
            .find(|k| k.get("id").and_then(|i| i.as_str()) == Some(id))
            .and_then(|k| k.get("name").and_then(|n| n.as_str()))
            .unwrap_or("")
            .to_string()
    };
    let mut t = SyTask {
        item_id: it.get("id").and_then(|i| i.as_str()).unwrap_or("").into(),
        title: String::new(),
        status: String::new(),
        branch: String::new(),
        pr: String::new(),
        body_doc: None,
    };
    for cell in it.get("cells").and_then(|c| c.as_array()).cloned().unwrap_or_default() {
        let kid = cell
            .pointer("/value/keyID")
            .and_then(|k| k.as_str())
            .unwrap_or("");
        let name = key_name(kid);
        let value = cell_value(&cell);
        match name.as_str() {
            "Primary Key" | "Title" | "Название" | "Первичный ключ" => {
                t.title = value;
                t.body_doc = cell
                    .pointer("/value/block/id")
                    .and_then(|i| i.as_str())
                    .map(String::from);
            }
            "Status" | "Статус" => t.status = value,
            "Branch" => t.branch = value,
            "PR" | "Pr" => t.pr = value,
            _ => {}
        }
    }
    if t.title.is_empty() {
        None
    } else {
        Some(t)
    }
}

fn cell_value(cell: &Value) -> String {
    let c = cell.get("value").unwrap_or(&Value::Null);
    if let Some(s) = c.as_str() {
        return s.to_string();
    }
    // ячейка несёт активный type: после конвертации ключа (text→url) в значении
    // остаётся stale text.content — читать нужно только поле активного типа
    let single = |ptr: &str| -> Option<String> {
        c.pointer(ptr).and_then(|s| s.as_str()).map(String::from)
    };
    let by_type = match c.get("type").and_then(|t| t.as_str()) {
        Some("block") => single("/block/content"),
        Some("text") => single("/text/content"),
        Some("url") => single("/url/content"),
        Some("email") => single("/email/content"),
        Some("phone") => single("/phone/content"),
        Some("select") | Some("mSelect") => {
            return c
                .get("mSelect")
                .and_then(|a| a.as_array())
                .map(|options| select_contents(options))
                .unwrap_or_default();
        }
        _ => None,
    };
    if let Some(v) = by_type {
        return v;
    }
    // фоллбек для значений без type: старый порядок указателей
    for ptr in ["/block/content", "/select/content", "/text/content", "/url/content", "/email/content", "/phone/content"] {
        if let Some(s) = single(ptr) {
            return s;
        }
    }
    c.get("mSelect")
        .and_then(|a| a.as_array())
        .map(|options| select_contents(options))
        .unwrap_or_default()
}

fn select_contents(options: &[Value]) -> String {
    options
        .iter()
        .filter_map(|o| o.get("content").and_then(|x| x.as_str()))
        .collect::<Vec<_>>()
        .join(",")
}

/// Выбор дока тела: живая привязка строки выигрывает (id-first); иначе —
/// док по заголовку (0 → нет тела, 1 → он, ≥2 → явная ошибка-дубль).
fn choose_body_doc(bound: Option<&str>, bound_is_live: bool, title_doc_ids: &[String]) -> Result<Option<String>> {
    if let Some(b) = bound {
        if bound_is_live {
            return Ok(Some(b.to_string()));
        }
    }
    match title_doc_ids {
        [] => Ok(None),
        [only] => Ok(Some(only.clone())),
        many => bail!("несколько доков с этим заголовком (тела-дубли): {}; вычисти сироты в UI", many.join(", ")),
    }
}

/// Живость дока через MCP block get, не SQL: индекс blocks отстаёт от
/// createDocWithMd — в окне лага SQL «не находит» свежий док и скидывает
/// строку на title-fallback с ложным предупреждением (репро ревью 04.11).
async fn doc_block_exists(sy: &SiYuan, doc_id: &str) -> bool {
    let Ok(text) = sy
        .mcp_tool_text("block", serde_json::json!({ "action": "get", "id": doc_id }))
        .await
    else {
        return false;
    };
    text.contains("Type: NodeDocument")
}

/// Доки по заголовку задачи (саб-доки доски): 0 → нет тела, 1 → он,
/// ≥2 → ошибка со списком docId (молчаливый первый — источник сирот).
pub async fn body_doc_by_title(sy: &SiYuan, board: &SyBoard, title: &str) -> Result<Vec<String>> {
    let hpath = board.body_hpath(title);
    sy.doc_ids_by_hpath(&board.notebook, &hpath).await
}

/// docId тела задачи: привязка строки (value.block.id), если жива;
/// fallback по заголовку только для detached-строк или мёртвой привязки
/// (с предупреждением — молчаливый hpath-путь маскирует поломку привязки).
pub async fn task_body_doc(sy: &SiYuan, board: &SyBoard, task: &SyTask) -> Result<Option<String>> {
    let title_ids = body_doc_by_title(sy, board, &task.title).await?;
    let bound = task.body_doc.as_deref();
    let bound_live = match bound {
        Some(b) => doc_block_exists(sy, b).await,
        None => false,
    };
    if bound.is_some() && !bound_live {
        eprintln!(
            "предупреждение: привязка строки {} мертва (docId {} не существует) — работаю по заголовку",
            task.item_id,
            bound.unwrap_or_default()
        );
    }
    choose_body_doc(bound, bound_live, &title_ids)
}

/// docId строк с мёртвой привязкой (для маркера (!) в task list).
/// Потолок пакета — 500 id (запрос одним SQL): на боевых досках не
/// достигается; при превышении хвост доски молча останется без маркировки.
pub async fn dead_bound_doc_ids(sy: &SiYuan, rows: &[SyTask]) -> Result<HashSet<String>> {
    let bound: Vec<&str> = rows.iter().filter_map(|r| r.body_doc.as_deref()).collect();
    if bound.is_empty() {
        return Ok(HashSet::new());
    }
    let list = bound.iter().map(|b| format!("'{b}'")).collect::<Vec<_>>().join(",");
    // явный limit: у ядра дефолт-лимит 64 строки — без него хвост выглядит мёртвым
    let live = sy
        .sql(&format!(
            "select id from blocks where type='d' and id in ({list}) limit 500"
        ))
        .await?;
    let live_set: HashSet<String> = live
        .iter()
        .filter_map(|r| r.get("id").and_then(|i| i.as_str()).map(String::from))
        .collect();
    Ok(bound
        .into_iter()
        .filter(|b| !live_set.contains(*b))
        .map(String::from)
        .collect())
}

/// ключ Status в database.
async fn status_key(sy: &SiYuan, av: &str) -> Result<String> {
    let v = sy.mcp_tool("database", serde_json::json!({ "action": "keys", "id": av })).await?;
    let keys = v
        .pointer("/data/keys")
        .or_else(|| v.get("keys"))
        .and_then(|k| k.as_array())
        .cloned()
        .unwrap_or_default();
    keys.iter()
        .find(|k| k.get("name").and_then(|n| n.as_str()) == Some("Status"))
        .and_then(|k| k.get("id").and_then(|i| i.as_str()).map(String::from))
        .ok_or_else(|| anyhow!("ключ Status не найден в database"))
}

pub fn body_template(title: &str, ty: &str, ctx: &str) -> String {
    match ty {
        "spike" => format!(
            "# {title}\n\n## Контекст\n{ctx}\n\n## Варианты\n<что рассматривали: суть → плюсы/минусы/замеры; сравнение — таблица>\n\n## Вывод\n<рекомендация + доказательства: «путь A, потому что …»>\n\n## Границы\n<что НЕ исследовано и почему; где вывод не действует>\n"
        ),
        "bug" => format!(
            "# {title}\n\n## Сценарий (BDD)\n```gherkin\nСценарий: <название>\n  Дано <состояние мира>\n  Когда <действие>\n  Тогда <ожидание>  # факт: <что было на самом деле>\n```\n\n{ctx}\n\n## Красный тест\n<имя теста, который воспроизводит; не воспроизвёлся на стенде → детали окружения>\n\n## Причина\n<механизм: почему система так себя вела>\n\n## Фикс\n<что изменилось; красный тест теперь зелёный>\n\n## Профилактика\n<как класс бага закрыт: тест/инвариант/линтер>\n"
        ),
        _ => format!(
            "# {title}\n\n## Постановка\n{ctx}\n\n## Решение\n<принципиально, как для нового инженера: как теперь устроено и почему так — концепция, без файлов>\n\n## Юзкейсы\n<сценарии: кто что делает>\n\n## Сиквенсы\n<взаимодействия текстом, по строке на шаг: А → Б: что>\n\n## Проверка\n<исполняемое: команды, тесты, живой прогон>\n"
        ),
    }
}

/// Создание задачи: строка Draft + тело-саб-док (идемпотентно по заголовку).
pub async fn create(
    sy: &SiYuan,
    project: &str,
    title: &str,
    ty: &str,
    ctx: &str,
) -> Result<(String, String)> {
    let board = find_board(sy, project).await?;
    // идемпотентность: строка или тело с таким заголовком уже есть?
    // сравнение через нормализацию: ретрай с любым написанием слэша — дубль
    let rows = read_rows(sy, &board).await?;
    let canon = normalize_title(title);
    if rows.iter().any(|r| normalize_title(&r.title) == canon) {
        bail!("живая строка с тайтлом «{title}» уже есть на доске — дубль");
    }
    // идемпотентность ретрая (ADR-009 «достроить сироту»): строка потерялась,
    // а тело-сирота цело → перепривязываем строку к нему, docId сохраняется
    let title_ids = body_doc_by_title(sy, &board, title).await?;
    let reused = choose_body_doc(None, false, &title_ids)?;
    // тело сначала (или сирота), затем СТРОКА ПРИВЯЗАНАЯ к нему (blockID):
    // клик по карточке открывает документ; detached-строки не создаём
    let body_path = board.body_hpath(title);
    let body = match reused {
        Some(existing) => existing,
        None => sy.create_doc_md(&board.notebook, &body_path, &body_template(title, ty, ctx)).await?,
    };
    // вставка строки сразу после createDocWithMd ненадёжна (репро 04.11 08:27):
    // success-ответ item_add не гарантирует строку в render — verify по itemID
    // с ретраем ×1; дедуп по тайтлу не даёт ретраю минтить вторую строку
    let sk = status_key(sy, &board.av_id).await?;
    let mut item = sy.db_item_add_bound(&board.av_id, &body).await?;
    sy.db_item_set_status(&board.av_id, &sk, &item, "Draft").await?;
    for attempt in 0..=1u8 {
        let rows = read_rows(sy, &board).await?;
        match check_row_insert(&rows, &item, title) {
            RowInsertCheck::Confirmed => {
                // тип — тег в теле
                if !ty.is_empty() {
                    let _ = sy.append_block_md(&body, &format!("\n#{ty}\n")).await;
                }
                return Ok((item, body));
            }
            RowInsertCheck::DuplicateTitle => bail!(
                "строка «{title}» уже видна в render с другим itemID — вставка {item} не подтвердилась, вторую строку не создаю"
            ),
            RowInsertCheck::Missing if attempt == 0 => {
                tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                item = sy.db_item_add_bound(&board.av_id, &body).await?;
                sy.db_item_set_status(&board.av_id, &sk, &item, "Draft").await?;
            }
            RowInsertCheck::Missing => bail!(
                "строка задачи «{title}» не появилась в database после вставки и ретрая (item {item}, тело {body}); задача видна только как док"
            ),
        }
    }
    unreachable!("цикл verify завершается return или bail")
}

/// Итог вставки строки: id найден / тайтл занят другой строкой / строки нет.
fn check_row_insert(rows: &[SyTask], item: &str, title: &str) -> RowInsertCheck {
    if rows.iter().any(|r| r.item_id == item) {
        return RowInsertCheck::Confirmed;
    }
    if rows.iter().any(|r| r.title == title) {
        return RowInsertCheck::DuplicateTitle;
    }
    RowInsertCheck::Missing
}

enum RowInsertCheck {
    Confirmed,
    DuplicateTitle,
    Missing,
}

/// Резолв задачи по id (itemID/docID/префикс/хвост) или подстроке заголовка.
pub async fn resolve(sy: &SiYuan, board: &SyBoard, needle: &str) -> Result<SyTask> {
    let needle = needle.trim_start_matches('-');
    let rows = read_rows(sy, board).await?;
    let by_id: Vec<&SyTask> = rows
        .iter()
        .filter(|r| r.item_id == needle || r.item_id.starts_with(needle) || r.item_id.ends_with(needle))
        .collect();
    if by_id.len() == 1 {
        return Ok(by_id[0].clone());
    }
    if !by_id.is_empty() {
        bail!("неоднозначный id: {} совпадений", by_id.len());
    }
    // тело по префиксу docId: одна выборка саб-доков доски вместо запроса на строку
    let like = board.hpath.replace('\'', "''");
    let subdocs = sy
        .sql(&format!(
            "select id from blocks where type='d' and hpath like '{like}/%' limit 500"
        ))
        .await?;
    let mut by_doc_prefix: Vec<&SyTask> = Vec::new();
    for r in &rows {
        let Some(doc) = r.body_doc.as_deref() else {
            continue;
        };
        if subdocs
            .iter()
            .any(|s| s.get("id").and_then(|i| i.as_str()) == Some(doc) && doc.starts_with(needle))
        {
            by_doc_prefix.push(r);
        }
    }
    if by_doc_prefix.len() == 1 {
        return Ok(by_doc_prefix[0].clone());
    }
    if by_doc_prefix.len() > 1 {
        bail!("неоднозначный doc-id префикс: {} совпадений", by_doc_prefix.len());
    }
    let by_title: Vec<&SyTask> = rows
        .iter()
        .filter(|r| normalize_title(&r.title).contains(&normalize_title(needle)))
        .collect();
    if by_title.len() == 1 {
        return Ok(by_title[0].clone());
    }
    if by_title.len() > 1 {
        bail!("неоднозначная подстрока: {} совпадений", by_title.len());
    }
    bail!("задача «{needle}» не найдена")
}

/// Коммент: строка в «## Комменты» тела задачи (реплай — отступ).
pub async fn comment(sy: &SiYuan, board: &SyBoard, task: &SyTask, author: &str, text: &str, reply_to: Option<&str>) -> Result<()> {
    let body = task_body_doc(sy, board, task)
        .await?
        .ok_or_else(|| anyhow!("тело задачи «{}» не найдено", task.title))?;
    let ts = chrono::Utc::now().format("%Y-%m-%d %H:%M");
    let line = match reply_to {
        Some(_) => format!("\n  - **{author}** ({ts}): {text}"),
        None => format!("\n- **{author}** ({ts}): {text}"),
    };
    sy.append_block_md(&body, &line).await?;
    Ok(())
}

/// Закрытие: статус Done (+note комментом).
pub async fn close(sy: &SiYuan, board: &SyBoard, task: &SyTask, note: Option<&str>) -> Result<()> {
    let sk = status_key(sy, &board.av_id).await?;
    sy.db_item_set_status(&board.av_id, &sk, &task.item_id, "Done").await?;
    if let Some(n) = note {
        let _ = comment(sy, board, task, "AGENTS", &format!("закрыта: {n}"), None).await;
    }
    Ok(())
}

/// Значение PR-ячейки (ключ типа url): пустая строка — очистка, иначе http(s)-ссылка.
fn pr_cell_value(pr: &str) -> Result<Value> {
    let pr = pr.trim();
    if pr.is_empty() {
        return Ok(serde_json::json!({ "url": { "content": "" } }));
    }
    if !pr.starts_with("http://") && !pr.starts_with("https://") {
        bail!("ключ PR хранит ссылку (url): ожидаю http(s)://…, получено «{pr}» (ветку пиши в --branch)");
    }
    Ok(serde_json::json!({ "url": { "content": pr } }))
}

/// Смена статуса/branch/pr (title-переименование не поддерживаем v1).
pub async fn edit(sy: &SiYuan, board: &SyBoard, task: &SyTask, status: Option<&str>, branch: Option<&str>, pr: Option<&str>) -> Result<()> {
    // валидация PR до любых записей: без частичных обновлений
    let pr_value = pr.map(pr_cell_value).transpose()?;
    let keys = sy.mcp_tool("database", serde_json::json!({ "action": "keys", "id": board.av_id })).await?;
    let arr = keys
        .pointer("/data/keys")
        .or_else(|| keys.get("keys"))
        .and_then(|k| k.as_array())
        .cloned()
        .unwrap_or_default();
    let find = |name: &str| -> Option<String> {
        arr.iter()
            .find(|k| k.get("name").and_then(|n| n.as_str()) == Some(name))
            .and_then(|k| k.get("id").and_then(|i| i.as_str()).map(String::from))
    };
    if let Some(st) = status {
        let k = find("Status").context("ключ Status не найден")?;
        sy.db_item_set_status(&board.av_id, &k, &task.item_id, st).await?;
    }
    if let Some(v) = branch {
        let k = find("Branch").context("ключ Branch не найден")?;
        sy.mcp_tool(
            "database",
            serde_json::json!({ "action": "item_update", "id": board.av_id, "keyID": k,
                    "itemID": task.item_id, "value": { "text": { "content": v } } }),
        )
        .await?;
    }
    if let Some(value) = pr_value {
        let k = find("PR").context("ключ PR не найден")?;
        sy.mcp_tool(
            "database",
            serde_json::json!({ "action": "item_update", "id": board.av_id, "keyID": k,
                    "itemID": task.item_id, "value": value }),
        )
        .await?;
    }
    Ok(())
}

/// Доска кронов: док /<project>/Кроны <project> с database-блоком.
pub async fn find_board_cron(sy: &SiYuan, project: &str) -> Result<SyBoard> {
    find_board_titled(sy, project, &format!("Кроны {project}")).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn probe_board() -> SyBoard {
        SyBoard {
            doc_id: "board-doc".into(),
            av_id: "board-av".into(),
            block_id: "board-block".into(),
            hpath: "/uwuwu/Задачи uwuwu".into(),
            notebook: "projects-nb".into(),
        }
    }

    #[test]
    fn path_safe_replaces_slash_with_division_slash() {
        assert_eq!(path_safe("вынести сборки из /tmp на /home"), "вынести сборки из \u{2215}tmp на \u{2215}home");
    }

    #[test]
    fn path_safe_leaves_text_without_slash_untouched() {
        assert_eq!(path_safe("обычный тайтл (v2)"), "обычный тайтл (v2)");
        assert_eq!(path_safe("уже ∕ санитизирован"), "уже ∕ санитизирован");
    }

    #[test]
    fn body_hpath_composes_board_hpath_with_sanitized_title() {
        let board = probe_board();
        assert_eq!(
            board.body_hpath("KO/VI: убрать оверрейсы"),
            "/uwuwu/Задачи uwuwu/KO\u{2215}VI: убрать оверрейсы"
        );
    }

    #[test]
    fn body_hpath_is_idempotent_on_sanitized_title() {
        let board = probe_board();
        let title = "Deploy: up\u{2215}down gateway";
        assert_eq!(board.body_hpath(title), board.body_hpath(&path_safe(title)));
    }

    #[test]
    fn normalize_title_matches_ascii_slash_needle_against_division_slash_title() {
        assert_eq!(normalize_title("Пилот RU\u{2215}EN"), normalize_title("пилот ru/en"));
    }

    #[test]
    fn normalize_title_keeps_distinct_titles_distinct() {
        assert_ne!(normalize_title("a/b"), normalize_title("a\u{2215}b/c"));
        assert_ne!(normalize_title("a/b"), normalize_title("ab"));
    }

    #[test]
    fn extract_av_id_reads_dash_case_attribute_view_block() {
        let md = r#"<div data-type="NodeAttributeView" data-av-id="20261104093449-q2h9ju2" data-av-type="kanban"></div>"#;
        assert_eq!(extract_av_id(md).as_deref(), Some("20261104093449-q2h9ju2"));
    }

    #[test]
    fn extract_av_id_reads_camel_case_carrier_block() {
        let md = r#"<div avID="20261104000238-96ch3l2"></div>"#;
        assert_eq!(extract_av_id(md).as_deref(), Some("20261104000238-96ch3l2"));
    }

    #[test]
    fn extract_av_id_returns_none_without_av_marker() {
        let md = r#"<div data-type="NodeDocument"></div>"#;
        assert_eq!(extract_av_id(md), None);
    }

    #[test]
    fn pick_search_hit_takes_exact_name_and_hpath() {
        let results = vec![json!({
            "avID": "av-1", "avName": "Задачи 1xgames", "blockID": "b-1",
            "hPath": "Projects/1xgames/Задачи 1xgames"
        })];
        let hit = pick_search_hit(&results, "Projects/1xgames/Задачи 1xgames", "Задачи 1xgames", "doc-x");
        assert_eq!(hit, Some(("av-1".into(), "b-1".into())));
    }

    #[test]
    fn pick_search_hit_skips_foreign_first_hit() {
        let results = vec![
            json!({
                "avID": "av-k", "avName": "Кроны 1xgames", "blockID": "b-k",
                "hPath": "Projects/1xgames/Кроны 1xgames"
            }),
            json!({
                "avID": "av-1", "avName": "Задачи 1xgames", "blockID": "b-1",
                "hPath": "Projects/1xgames/Задачи 1xgames"
            }),
        ];
        let hit = pick_search_hit(&results, "Projects/1xgames/Задачи 1xgames", "Задачи 1xgames", "doc-x");
        assert_eq!(hit, Some(("av-1".into(), "b-1".into())));
    }

    #[test]
    fn pick_search_hit_returns_none_without_valid_hits() {
        let results = vec![json!({
            "avID": "av-k", "avName": "Кроны 1xgames", "blockID": "b-k",
            "hPath": "Projects/1xgames/Кроны 1xgames"
        })];
        let hit = pick_search_hit(&results, "Projects/1xgames/Задачи 1xgames", "Задачи 1xgames", "doc-x");
        assert_eq!(hit, None);
    }

    #[test]
    fn pick_search_hit_accepts_hit_anchored_by_block_id() {
        let results = vec![json!({
            "avID": "av-9", "avName": "Задачи 1xgames", "blockID": "doc-77",
            "hPath": "Другой/путь"
        })];
        let hit = pick_search_hit(&results, "Projects/1xgames/Задачи 1xgames", "Задачи 1xgames", "doc-77");
        assert_eq!(hit, Some(("av-9".into(), "doc-77".into())));
    }

    #[test]
    fn choose_body_doc_prefers_live_binding_over_title() {
        let title_ids = vec!["doc-a".to_string(), "doc-b".to_string()];
        let chosen = choose_body_doc(Some("doc-bound"), true, &title_ids).unwrap();
        assert_eq!(chosen.as_deref(), Some("doc-bound"));
    }

    #[test]
    fn choose_body_doc_falls_back_to_title_when_binding_dead() {
        let title_ids = vec!["doc-a".to_string()];
        let chosen = choose_body_doc(Some("doc-dead"), false, &title_ids).unwrap();
        assert_eq!(chosen.as_deref(), Some("doc-a"));
    }

    #[test]
    fn choose_body_doc_errors_listing_duplicates_in_fallback() {
        let title_ids = vec!["doc-a".to_string(), "doc-b".to_string()];
        let err = choose_body_doc(Some("doc-dead"), false, &title_ids).unwrap_err();
        assert!(err.to_string().contains("doc-a"));
        assert!(err.to_string().contains("doc-b"));
    }

    #[test]
    fn choose_body_doc_returns_none_without_binding_and_docs() {
        let chosen = choose_body_doc(None, false, &[]).unwrap();
        assert_eq!(chosen, None);
    }

    fn render_row(title: &str, row_id: &str, bound_doc: Option<&str>) -> Value {
        let mut block = json!({ "content": title });
        if let Some(doc) = bound_doc {
            block["id"] = json!(doc);
        }
        json!({
            "id": row_id,
            "cells": [{
                "value": {
                    "keyID": "key-1",
                    "type": "block",
                    "block": block
                }
            }]
        })
    }

    #[test]
    fn parse_render_row_reads_bound_doc_from_primary_cell() {
        let keys = vec![json!({ "id": "key-1", "name": "Primary Key" })];
        let row = render_row("таск", "row-1", Some("doc-9"));
        let t = parse_render_row(&row, &keys).unwrap();
        assert_eq!(t.title, "таск");
        assert_eq!(t.item_id, "row-1");
        assert_eq!(t.body_doc.as_deref(), Some("doc-9"));
    }

    #[test]
    fn parse_render_row_keeps_detached_row_without_body_doc() {
        let keys = vec![json!({ "id": "key-1", "name": "Первичный ключ" })];
        let row = render_row("таск", "row-2", None);
        let t = parse_render_row(&row, &keys).unwrap();
        assert_eq!(t.body_doc, None);
    }

    #[test]
    fn parse_render_row_skips_rows_without_title() {
        let keys = vec![json!({ "id": "key-1", "name": "Primary Key" })];
        let row = json!({ "id": "row-3", "cells": [] });
        assert!(parse_render_row(&row, &keys).is_none());
    }

    fn sy_task(item_id: &str, title: &str, body_doc: Option<&str>) -> SyTask {
        SyTask {
            item_id: item_id.to_string(),
            title: title.to_string(),
            status: String::new(),
            branch: String::new(),
            pr: String::new(),
            body_doc: body_doc.map(String::from),
        }
    }

    #[test]
    fn check_row_insert_confirms_inserted_row_by_item_id() {
        let rows = vec![
            sy_task("item-1", "первая", Some("doc-1")),
            sy_task("item-2", "вторая", Some("doc-2")),
        ];
        assert!(matches!(check_row_insert(&rows, "item-2", "новая"), RowInsertCheck::Confirmed));
    }

    #[test]
    fn check_row_insert_reports_duplicate_title_not_inserted_id() {
        let rows = vec![sy_task("item-x", "задача", Some("doc-x"))];
        assert!(matches!(check_row_insert(&rows, "item-y", "задача"), RowInsertCheck::DuplicateTitle));
    }

    #[test]
    fn check_row_insert_reports_missing_row() {
        let rows = vec![sy_task("item-1", "другая", Some("doc-1"))];
        assert!(matches!(check_row_insert(&rows, "item-9", "новая"), RowInsertCheck::Missing));
    }

    fn cell_of(value: Value) -> Value {
        json!({ "value": value })
    }

    #[test]
    fn cell_value_prefers_url_over_stale_text_on_url_typed_cell() {
        // после конвертации ключа text→url в значении остаётся старый text.content
        let cell = cell_of(json!({
            "type": "url",
            "text": { "content": "https://old.example/pull/1" },
            "url": { "content": "https://new.example/pull/2" }
        }));
        assert_eq!(cell_value(&cell), "https://new.example/pull/2");
    }

    #[test]
    fn cell_value_returns_empty_for_url_cell_with_unfilled_url() {
        // до-миграционный артефакт: url пуст, текст застрял в text — честно пусто
        let cell = cell_of(json!({
            "type": "url",
            "text": { "content": "https://old.example/pull/1" },
            "url": { "content": "" }
        }));
        assert_eq!(cell_value(&cell), "");
    }

    #[test]
    fn cell_value_reads_text_typed_cell() {
        let cell = cell_of(json!({ "type": "text", "text": { "content": "feature/x" } }));
        assert_eq!(cell_value(&cell), "feature/x");
    }

    #[test]
    fn cell_value_reads_block_typed_cell() {
        let cell = cell_of(json!({ "type": "block", "block": { "content": "Заголовок" } }));
        assert_eq!(cell_value(&cell), "Заголовок");
    }

    #[test]
    fn cell_value_joins_multi_select_options() {
        let cell = cell_of(json!({
            "type": "mSelect",
            "mSelect": [{ "content": "Done", "color": "1" }, { "content": "Todo", "color": "2" }]
        }));
        assert_eq!(cell_value(&cell), "Done,Todo");
    }

    #[test]
    fn cell_value_falls_back_to_pointers_without_type() {
        let cell = cell_of(json!({ "text": { "content": "без type" } }));
        assert_eq!(cell_value(&cell), "без type");
    }

    #[test]
    fn pr_cell_value_wraps_https_link_into_url_payload() {
        let v = pr_cell_value("https://github.com/yurvon-screamo/uwuwu-agent/pull/36").unwrap();
        assert_eq!(v, json!({ "url": { "content": "https://github.com/yurvon-screamo/uwuwu-agent/pull/36" } }));
    }

    #[test]
    fn pr_cell_value_accepts_http_link() {
        let v = pr_cell_value("http://gitea.local/origa/pull/9").unwrap();
        assert_eq!(v, json!({ "url": { "content": "http://gitea.local/origa/pull/9" } }));
    }

    #[test]
    fn pr_cell_value_trims_whitespace_around_link() {
        let v = pr_cell_value("  https://github.com/a/b/pull/1  ").unwrap();
        assert_eq!(v, json!({ "url": { "content": "https://github.com/a/b/pull/1" } }));
    }

    #[test]
    fn pr_cell_value_empty_string_clears_the_cell() {
        let v = pr_cell_value("").unwrap();
        assert_eq!(v, json!({ "url": { "content": "" } }));
    }

    #[test]
    fn pr_cell_value_rejects_non_link_input() {
        let err = pr_cell_value("main").unwrap_err().to_string();
        assert!(err.contains("http(s)://"), "неожиданное сообщение: {err}");
        assert!(err.contains("--branch"), "неожиданное сообщение: {err}");
    }
}
