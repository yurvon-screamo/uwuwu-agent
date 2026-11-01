//! AFFiNE-backed implementations of the read-side commands: read, projects,
//! grep and semantic search. Every command syncs its local content cache
//! from the workspace first, so any machine sees the whole base.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use serde_json::json;

use crate::affine_client::{AffineClient, AffineConfig};

/// Живой сбор тел задач/кронов по тегам — не зависит от дерева
/// (linked-тела невидимы list_workspace_tree). Cap guards от разросшейся доски.
/// Кэш тел задач/кронов для поисковой семантики (TTL 300с): 36+ живых
/// export-вызовов на каждый search — недопустимый множитель через туннель.
/// Свежий кэш = мгновенно; протухший = один живой проход + сохранение.
const TAGGED_CACHE_TTL_SECS: u64 = 300;

/// Загрузить tagged-кэш без шлюза (деградация поиска при лежащем MCP).
pub fn tagged_docs_cache_load(root: &Path) -> Vec<(String, String, String, &'static str)> {
    read_tagged_cache(root).unwrap_or_default()
}

fn read_tagged_cache(root: &Path) -> Option<Vec<(String, String, String, &'static str)>> {
    let raw = std::fs::read_to_string(root.join("tagged_docs_cache.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let rows: Vec<(String, String, String, String)> =
        serde_json::from_value(v.get("rows")?.clone()).ok()?;
    Some(
        rows.into_iter()
            .map(|(a, b, c, d)| {
                let kind: &'static str = match d.as_str() {
                    "cron" => "cron",
                    _ => "task",
                };
                (a, b, c, kind)
            })
            .collect(),
    )
}

pub async fn tagged_docs_cached(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    root: &Path,
) -> Vec<(String, String, String, &'static str)> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let path = root.join("tagged_docs_cache.json");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(raw) = std::fs::read_to_string(&path) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
            let ts = v.get("ts").and_then(|t| t.as_u64()).unwrap_or(0);
            if now.saturating_sub(ts) < TAGGED_CACHE_TTL_SECS {
                if let Ok(rows) = serde_json::from_value::<Vec<(String, String, String, String)>>(v)
                {
                    // типы статические восстанавливаем по 4-му полю
                    return rows
                        .into_iter()
                        .map(|(a, b, c, d)| {
                            let kind: &'static str = match d.as_str() {
                                "cron" => "cron",
                                _ => "task",
                            };
                            (a, b, c, kind)
                        })
                        .collect();
                }
            }
        }
    }
    let rows = tagged_docs(client, cfg).await;
    // пустой живой проход не кэшируем: моргающий апстрим не должен
    // губить семантику задач на 5 минут
    if !rows.is_empty() {
        let payload = serde_json::json!({
            "ts": now,
            "rows": rows.iter()
                .map(|(a, b, c, d)| (a, b, c, d.to_string()))
                .collect::<Vec<_>>(),
        });
        if let Ok(s) = serde_json::to_string(&payload) {
            let _ = std::fs::write(&path, s);
        }
    }
    rows
}

pub async fn tagged_docs(
    client: &mut AffineClient,
    cfg: &AffineConfig,
) -> Vec<(String, String, String, &'static str)> {
    const CAP: usize = 60;
    let mut out: Vec<(String, String, String, &'static str)> = Vec::new();
    for (tag, kind) in [
        ("Dev", "task"),
        ("Spike", "task"),
        ("Bug", "task"),
        ("Cron", "cron"),
    ] {
        let Ok(r) = client
            .call_tool(
                "list_docs_by_tag",
                json!({"workspaceId": cfg.workspace, "tag": tag}),
            )
            .await
        else {
            continue;
        };
        let docs = r
            .get("docs")
            .and_then(|d| d.as_array())
            .cloned()
            .unwrap_or_default();
        for d in docs {
            if out.len() >= CAP {
                return out;
            }
            let id = d
                .get("id")
                .or_else(|| d.get("docId"))
                .and_then(|v| v.as_str());
            let title = d.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let Some(id) = id else { continue };
            if out.iter().any(|(did, ..)| did == id) {
                continue;
            }
            if let Ok(e) = client
                .call_tool(
                    "export_doc_markdown",
                    json!({"workspaceId": cfg.workspace, "docId": id}),
                )
                .await
            {
                let md = e.get("markdown").and_then(|m| m.as_str()).unwrap_or("");
                out.push((id.to_string(), title.to_string(), md.to_string(), kind));
            }
        }
    }
    out
}

use crate::affine_docs::{read_markdown, resolve_slug};
use crate::content_cache::{ContentCache, DocKind};

/// Connect + sync the content cache. Every read command starts here — it is
/// what makes the CLI multi-PC safe.
pub async fn session(root: &Path, cfg: &AffineConfig) -> Result<(AffineClient, ContentCache)> {
    session_with_ttl(root, cfg, 600).await
}

/// A cache synced within the TTL is reused instead of re-walking the whole
/// workspace tree on every single read/grep.
pub async fn session_with_ttl(
    root: &Path,
    cfg: &AffineConfig,
    ttl_secs: u64,
) -> Result<(AffineClient, ContentCache)> {
    let mut client = AffineClient::connect(cfg).await?;
    let mut cache = ContentCache::load(root)?;
    let tree_blind = crate::content_cache::tree_disabled(root);
    if !cache.is_fresh(ttl_secs) && !tree_blind {
        // Слепой tree у ограниченного акка (бот-член: list_workspace_tree пуст)
        // НЕ должен ронять команду: работаем с локальным кэшем, резолв добирает
        // через search_docs (resolve_doc_id last resort). Пустой кэш — честная ошибка.
        match cache.sync(&mut client, cfg, root).await {
            Ok(report) => {
                if report.fetched > 0 {
                    eprintln!("affine: content cache +{} docs", report.fetched);
                }
            }
            Err(e) if cache.iter().count() > 0 => {
                eprintln!(
                    "affine: синк пропущен ({e:#}) — работаем с локальным кэшем ({} доков)",
                    cache.iter().count()
                );
            }
            Err(e) => return Err(e),
        }
    }
    Ok((client, cache))
}

// ---------- read ----------

pub async fn read(root: &Path, cfg: &AffineConfig, slug: &str) -> Result<String> {
    let (mut client, cache) = session(root, cfg).await?;
    if let Some(doc) = cache.get(slug) {
        let mut md = doc.markdown.clone();
        if let Some(summary) = rich_blocks_section(&mut client, cfg, &doc.doc_id).await {
            md.push_str(&summary);
        }
        return Ok(annotate_blob_links(md));
    }
    // cache miss on a fresh doc: resolve by tag and read directly
    let mut map = crate::affine_store::MigrationMap::load_for(root, cfg)?;
    if let Some(doc_id) = resolve_slug(&mut client, &mut map, cfg, slug).await? {
        let mut md = read_markdown(&mut client, cfg, &doc_id).await?;
        if let Some(summary) = rich_blocks_section(&mut client, cfg, &doc_id).await {
            md.push_str(&summary);
        }
        return Ok(annotate_blob_links(md));
    }
    anyhow::bail!("article not found by slug: '{slug}'")
}

/// Resolve a slug/title to a doc id (cache → migration map → gateway title
/// search). Shared by commands that need the id without the markdown body.
pub async fn resolve_doc_id(root: &Path, cfg: &AffineConfig, slug: &str) -> Result<Option<String>> {
    let (mut client, cache) = session(root, cfg).await?;
    if let Some(doc) = cache.get(slug) {
        return Ok(Some(doc.doc_id.clone()));
    }
    let mut map = crate::affine_store::MigrationMap::load_for(root, cfg)?;
    if let Some(doc_id) = resolve_slug(&mut client, &mut map, cfg, slug).await? {
        return Ok(Some(doc_id));
    }
    // last resort: title search through the gateway — exact match only,
    // so a mistyped slug never resolves to a different document
    let resp = client
        .call_tool(
            "search_docs",
            json!({"workspaceId": cfg.workspace, "query": slug, "limit": 5}),
        )
        .await;
    if let Ok(resp) = resp {
        if let Some(results) = resp.get("results").and_then(|r| r.as_array()) {
            for r in results {
                if r.get("inTrash").and_then(|t| t.as_bool()) == Some(false) {
                    let exact = r.get("title").and_then(|t| t.as_str()) == Some(slug);
                    if exact {
                        if let Some(id) = r.get("docId").and_then(|d| d.as_str()) {
                            return Ok(Some(id.to_string()));
                        }
                    }
                }
            }
        }
    }
    Ok(None)
}

/// Rewrite `affine://blob/<key>` image links into a download hint — the
/// scheme is not fetchable; `uwuwu-cli blob <key>` fetches the bytes.
fn annotate_blob_links(md: String) -> String {
    if !md.contains("affine://blob/") {
        return md;
    }
    static BLOB_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = BLOB_RE
        .get_or_init(|| regex::Regex::new(r"!\[([^\]]*)\]\(affine://blob/([^)\s]+)\)").unwrap());
    re.replace_all(&md, |caps: &regex::Captures| {
        format!(
            "**[image: {} (key: {})] → скачай: `uwuwu-cli blob {} > файл`**",
            caps.get(1).map(|m| m.as_str()).unwrap_or(""),
            caps.get(2).map(|m| m.as_str()).unwrap_or(""),
            caps.get(2).map(|m| m.as_str()).unwrap_or("")
        )
    })
    .to_string()
}

/// Render non-markdown blocks (databases, mindmaps, canvas) into a text
/// appendix so `read` is self-sufficient: markdown export cannot represent
/// them, but the structural APIs can. One blocks-only gateway call per read;
/// databases/canvas/mindmaps trigger extra calls only when present.
async fn rich_blocks_section(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    doc_id: &str,
) -> Option<String> {
    let resp = match client
        .call_tool(
            "read_doc",
            json!({"workspaceId": cfg.workspace, "docId": doc_id, "includeMarkdown": false}),
        )
        .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("affine: проверка rich-блоков не удалась ({e:#}); приложение пропущено");
            return None;
        }
    };
    let blocks = resp.get("blocks").and_then(|b| b.as_array())?;
    let mut database_ids: Vec<String> = Vec::new();
    let mut has_surface = false;
    for block in blocks {
        match block.get("flavour").and_then(|f| f.as_str()) {
            Some("affine:database") => {
                if let Some(id) = block.get("id").and_then(|i| i.as_str()) {
                    database_ids.push(id.to_string());
                }
            }
            Some("affine:surface") => has_surface = true,
            _ => {}
        }
    }
    if !has_surface && database_ids.is_empty() {
        return None;
    }
    let mut sections: Vec<String> = Vec::new();

    // --- databases → GFM tables
    for db_id in &database_ids {
        match render_database(client, cfg, doc_id, db_id).await {
            Ok(Some(table)) => sections.push(table),
            Ok(None) => {}
            Err(e) => {
                eprintln!("affine: база {db_id} не прочитана ({e:#})");
                sections.push(format!("### 📊 База {db_id} (не прочитана)\n"));
            }
        }
    }

    // --- canvas: mindmaps → trees, standalone shapes/connectors/text → list
    if has_surface {
        match client
            .call_tool(
                "get_edgeless_canvas",
                json!({"workspaceId": cfg.workspace, "docId": doc_id}),
            )
            .await
        {
            Ok(canvas) => {
                if let Some(canvas_section) = render_canvas(client, cfg, doc_id, &canvas).await {
                    sections.push(canvas_section);
                }
            }
            Err(e) => {
                eprintln!("affine: канва не прочитана ({e:#}); приложение канвы пропущено");
            }
        }
    }

    if sections.is_empty() {
        return None;
    }
    Some(format!("\n\n---\n{}\n", sections.join("\n\n")))
}

/// Read one database block and render it as a GFM table.
async fn render_database(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    doc_id: &str,
    db_id: &str,
) -> Result<Option<String>> {
    let cols_resp = client
        .call_tool(
            "read_database_columns",
            json!({"workspaceId": cfg.workspace, "docId": doc_id, "databaseBlockId": db_id}),
        )
        .await?;
    let cells_resp = client
        .call_tool(
            "read_database_cells",
            json!({"workspaceId": cfg.workspace, "docId": doc_id, "databaseBlockId": db_id}),
        )
        .await?;
    let Some(columns) = cols_resp.get("columns").and_then(|c| c.as_array()) else {
        return Ok(None);
    };
    let rows = cells_resp.get("rows").and_then(|r| r.as_array());
    let title = cols_resp
        .get("title")
        .and_then(|t| t.as_str())
        .unwrap_or("база");
    let mut out = format!(
        "### 📊 {title} ({} строк × {} колонок)\n\n",
        rows.map(|r| r.len()).unwrap_or(0),
        columns.len()
    );
    let col_names: Vec<String> = columns
        .iter()
        .map(|c| {
            c.get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("?")
                .to_string()
        })
        .collect();
    let header = format!("| {} |", col_names.join(" | "));
    out.push_str(&header);
    out.push('\n');
    out.push_str(&format!("|{}|", vec![" --- "; col_names.len()].join("|")));
    out.push('\n');
    if let Some(rows) = rows {
        for row in rows {
            let mut cells: Vec<String> = Vec::with_capacity(col_names.len());
            for name in &col_names {
                let raw = if name.eq_ignore_ascii_case("title") {
                    row.get("title").cloned().unwrap_or(serde_json::Value::Null)
                } else {
                    row.pointer(&format!("/cells/{}/value", name))
                        .cloned()
                        .unwrap_or(serde_json::Value::Null)
                };
                cells.push(
                    tolerant_cell_text(&raw)
                        .replace('|', "\\|")
                        .replace('\n', " "),
                );
            }
            out.push_str(&format!("| {} |\n", cells.join(" | ")));
        }
    }
    Ok(Some(out))
}

/// Cell value → text: strings as-is, numbers via their JSON form, rich-text
/// arrays ([{text: ".."}] or [{insert: ".."}]) joined — anything else empty.
fn tolerant_cell_text(value: &serde_json::Value) -> String {
    if let Some(s) = value.as_str() {
        return s.to_string();
    }
    if let Some(n) = value.as_f64() {
        return if n.fract() == 0.0 {
            format!("{}", n as i64)
        } else {
            format!("{n}")
        };
    }
    if let Some(arr) = value.as_array() {
        let mut parts = Vec::new();
        for item in arr {
            let t = item
                .get("text")
                .or_else(|| item.get("insert"))
                .and_then(|t| t.as_str())
                .unwrap_or("");
            if !t.is_empty() {
                parts.push(t.to_string());
            }
        }
        return parts.join(" ");
    }
    if let Some(b) = value.as_bool() {
        return b.to_string();
    }
    String::new()
}

/// Render canvas: mindmap trees (get_mindmap) + standalone shapes, text and
/// connectors with endpoints resolved to shape texts. Mindmap member shapes
/// are excluded from the standalone list to avoid double listing.
/// Собранные элементы канвы для рендера: сбор и рендер разнесены, чтобы
/// каждый шаг читался отдельно.
struct CanvasIndex {
    mindmap_ids: Vec<String>,
    /// id → (shapeType, text)
    shapes_by_id: std::collections::BTreeMap<String, (String, String)>,
    texts: Vec<String>,
    /// (from, to, label)
    connectors: Vec<(String, String, String)>,
}

impl CanvasIndex {
    fn is_empty(&self) -> bool {
        self.mindmap_ids.is_empty()
            && self.shapes_by_id.is_empty()
            && self.texts.is_empty()
            && self.connectors.is_empty()
    }
}

fn collect_canvas_elements(elements: &[serde_json::Value]) -> CanvasIndex {
    let mut idx = CanvasIndex {
        mindmap_ids: Vec::new(),
        shapes_by_id: std::collections::BTreeMap::new(),
        texts: Vec::new(),
        connectors: Vec::new(),
    };
    for el in elements {
        let id = el.get("id").and_then(|i| i.as_str()).unwrap_or_default();
        match el.get("type").and_then(|t| t.as_str()) {
            Some("mindmap") => idx.mindmap_ids.push(id.to_string()),
            Some("shape") => {
                let shape_type = el
                    .get("shapeType")
                    .and_then(|t| t.as_str())
                    .unwrap_or("shape");
                let text = el.get("text").and_then(|t| t.as_str()).unwrap_or("");
                idx.shapes_by_id
                    .insert(id.to_string(), (shape_type.to_string(), text.to_string()));
            }
            Some("text") => {
                if let Some(t) = el.get("text").and_then(|t| t.as_str()) {
                    idx.texts.push(t.to_string());
                }
            }
            Some("connector") => {
                let from = el
                    .pointer("/source/id")
                    .and_then(|s| s.as_str())
                    .unwrap_or("");
                let to = el
                    .pointer("/target/id")
                    .and_then(|s| s.as_str())
                    .unwrap_or("");
                let label = el
                    .get("label")
                    .and_then(|s| s.as_str())
                    .or_else(|| el.get("labelText").and_then(|s| s.as_str()))
                    .unwrap_or("");
                idx.connectors
                    .push((from.to_string(), to.to_string(), label.to_string()));
            }
            _ => {}
        }
    }
    idx
}

/// Canvas mindmap elements carry their member ids in `children` — the map
/// seeds member_ids so shapes do not duplicate even when the get_mindmap
/// call fails. Key: mindmap id; value: the mindmap id + its children keys.
fn canvas_mindmap_members(
    elements: &[serde_json::Value],
) -> std::collections::BTreeMap<String, Vec<String>> {
    elements
        .iter()
        .filter_map(|el| {
            let id = el.get("id").and_then(|i| i.as_str())?;
            if el.get("type").and_then(|t| t.as_str()) != Some("mindmap") {
                return None;
            }
            let mut ids: Vec<String> = vec![id.to_string()];
            if let Some(children) = el.get("children").and_then(|c| c.as_object()) {
                for cid in children.keys() {
                    ids.push(cid.clone());
                }
            }
            Some((id.to_string(), ids))
        })
        .collect()
}

/// Один майндмап канвы: get_mindmap + дерево. Пишет id узлов в member_ids
/// (узлы майндмапа = шейпы, их не надо дублировать списком шейпов);
/// ошибка чтения → пустая строка, рендер канвы продолжается.
async fn render_canvas_mindmap(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    doc_id: &str,
    mm_id: &str,
    canvas_members: Option<&Vec<String>>,
    member_ids: &mut std::collections::BTreeSet<String>,
) -> String {
    let mm = match client
        .call_tool(
            "get_mindmap",
            json!({"workspaceId": cfg.workspace, "docId": doc_id, "mindmapId": mm_id}),
        )
        .await
    {
        Ok(mm) => mm,
        Err(e) => {
            eprintln!("affine: майндмап {mm_id} не прочитан ({e:#})");
            if let Some(ids) = canvas_members {
                member_ids.extend(ids.iter().cloned());
            }
            return String::new();
        }
    };
    if let Some(ids) = canvas_members {
        member_ids.extend(ids.iter().cloned());
    }
    let root_id = mm
        .get("rootId")
        .and_then(|r| r.as_str())
        .unwrap_or_default();
    let mut text_by_id: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    let mut children_by_parent: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    if let Some(nodes) = mm.get("nodes").and_then(|n| n.as_array()) {
        for n in nodes {
            let Some(nid) = n.get("nodeId").and_then(|i| i.as_str()) else {
                continue;
            };
            member_ids.insert(nid.to_string());
            if let Some(t) = n.get("text").and_then(|t| t.as_str()) {
                text_by_id.insert(nid.to_string(), t.to_string());
            }
            if let Some(parent) = n.get("parentId").and_then(|p| p.as_str()) {
                children_by_parent
                    .entry(parent.to_string())
                    .or_default()
                    .push(nid.to_string());
            }
        }
    }
    if !root_id.is_empty() {
        member_ids.insert(root_id.to_string());
    }
    format!(
        "**Майндмап** ({} узлов):\n{}\n",
        text_by_id.len(),
        render_tree(
            root_id,
            &text_by_id,
            &children_by_parent,
            0,
            &mut std::collections::BTreeSet::new(),
        )
    )
}

/// Подпись конца коннектора: текст шейпа → текст канвы → сырой id.
fn connector_endpoint_label(
    id: &str,
    shapes_by_id: &std::collections::BTreeMap<String, (String, String)>,
    texts_by_id: &std::collections::BTreeMap<&str, &str>,
) -> String {
    shapes_by_id
        .get(id)
        .map(|(_, t)| t.clone())
        .filter(|t| !t.is_empty())
        .or_else(|| texts_by_id.get(id).map(|t| t.to_string()))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| id.to_string())
}

async fn render_canvas(
    client: &mut AffineClient,
    cfg: &AffineConfig,
    doc_id: &str,
    canvas: &serde_json::Value,
) -> Option<String> {
    let elements = canvas.get("surfaceElements").and_then(|e| e.as_array())?;
    let idx = collect_canvas_elements(elements);
    if idx.is_empty() {
        return None;
    }
    let mut out = String::from("### 🎨 Канва\n\n");
    let mut member_ids: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    let canvas_members = canvas_mindmap_members(elements);
    for mm_id in &idx.mindmap_ids {
        out.push_str(
            &render_canvas_mindmap(
                client,
                cfg,
                doc_id,
                mm_id,
                canvas_members.get(mm_id),
                &mut member_ids,
            )
            .await,
        );
    }

    for (id, (shape_type, text)) in &idx.shapes_by_id {
        if member_ids.contains(id) {
            continue; // mindmap node shapes are already in the tree
        }
        let label = if text.is_empty() {
            shape_type.clone()
        } else {
            text.clone()
        };
        out.push_str(&format!("- [{shape_type}] {label}\n"));
    }
    for t in &idx.texts {
        out.push_str(&format!("- 📝 {t}\n"));
    }
    let texts_by_id: std::collections::BTreeMap<&str, &str> = elements
        .iter()
        .filter_map(|el| {
            let id = el.get("id").and_then(|i| i.as_str())?;
            let t = el.get("text").and_then(|t| t.as_str())?;
            Some((id, t))
        })
        .collect();
    for (from, to, label) in &idx.connectors {
        let from_t = connector_endpoint_label(from, &idx.shapes_by_id, &texts_by_id);
        let to_t = connector_endpoint_label(to, &idx.shapes_by_id, &texts_by_id);
        if label.is_empty() {
            out.push_str(&format!("- 🔗 {from_t} → {to_t}\n"));
        } else {
            out.push_str(&format!("- 🔗 {from_t} → {to_t} ({label})\n"));
        }
    }
    Some(out)
}

const MAX_MINDMAP_DEPTH: usize = 32;

fn render_tree(
    node_id: &str,
    text_by_id: &std::collections::BTreeMap<String, String>,
    children_by_parent: &std::collections::BTreeMap<String, Vec<String>>,
    depth: usize,
    visited: &mut std::collections::BTreeSet<String>,
) -> String {
    // Cyclic parentId graphs are external data (editor/importer bugs):
    // a visited-set + depth cap keep the recursion bounded instead of
    // aborting the whole process with a stack overflow.
    if depth >= MAX_MINDMAP_DEPTH || !visited.insert(node_id.to_string()) {
        return if depth >= MAX_MINDMAP_DEPTH {
            format!("{}…\n", "  ".repeat(depth))
        } else {
            String::new()
        };
    }
    let text = text_by_id.get(node_id).cloned().unwrap_or_default();
    let mut out = format!("{}- {}\n", "  ".repeat(depth), text);
    if let Some(children) = children_by_parent.get(node_id) {
        for child in children {
            out.push_str(&render_tree(
                child,
                text_by_id,
                children_by_parent,
                depth + 1,
                visited,
            ));
        }
    }
    out
}

// ---------- projects ----------

pub struct ProjectRow {
    pub name: String,
    pub docs: usize,
    pub description: String,
}

pub fn projects(cache: &ContentCache) -> Vec<ProjectRow> {
    let mut by_project: BTreeMap<String, (usize, String)> = BTreeMap::new();
    for (slug, doc) in cache.docs_by_kind(&DocKind::Project) {
        let project = slug.split('/').next().unwrap_or(slug).to_string();
        let entry = by_project
            .entry(project.clone())
            .or_insert((0, String::new()));
        entry.0 += 1;
        if entry.1.is_empty() {
            entry.1 = first_meaningful_line(&doc.markdown);
        }
    }
    by_project
        .into_iter()
        .map(|(name, (docs, description))| ProjectRow {
            name,
            docs,
            description,
        })
        .collect()
}

fn first_meaningful_line(markdown: &str) -> String {
    markdown
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .unwrap_or("")
        .chars()
        .take(120)
        .collect()
}

// ---------- grep ----------

pub struct GrepHit {
    pub slug: String,
    pub line_number: usize,
    pub line: String,
}

pub fn grep(cache: &ContentCache, pattern: &str) -> Vec<GrepHit> {
    let needle = pattern.to_lowercase();
    let mut hits = Vec::new();
    for (slug, doc) in cache
        .docs_by_kind(&DocKind::Experience)
        .into_iter()
        .chain(cache.docs_by_kind(&DocKind::Project))
    {
        for (n, line) in doc.markdown.lines().enumerate() {
            if line.to_lowercase().contains(&needle) {
                hits.push(GrepHit {
                    slug: slug.clone(),
                    line_number: n + 1,
                    line: line.to_string(),
                });
            }
        }
    }
    hits
}

#[cfg(test)]
mod rich_render_tests {
    use super::{render_tree, tolerant_cell_text};
    use std::collections::{BTreeMap, BTreeSet};

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn kids(pairs: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
        pairs
            .iter()
            .map(|(k, v)| {
                (
                    k.to_string(),
                    v.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                )
            })
            .collect()
    }

    #[test]
    fn renders_nested_children() {
        let texts = map(&[("root", "Корень"), ("a", "Ветка A"), ("a1", "Лист A1")]);
        let children = kids(&[("root", &["a"]), ("a", &["a1"])]);
        let out = render_tree("root", &texts, &children, 0, &mut BTreeSet::new());
        assert!(out.contains("- Корень"));
        assert!(out.contains("  - Ветка A"));
        assert!(out.contains("    - Лист A1"));
    }

    #[test]
    fn survives_parent_cycle() {
        // a → b → a: unguarded recursion would loop forever / overflow.
        let texts = map(&[("a", "A"), ("b", "B")]);
        let children = kids(&[("a", &["b", "b", "a"]), ("b", &["a"])]);
        let out = render_tree("a", &texts, &children, 0, &mut BTreeSet::new());
        assert_eq!(out.matches("- A").count(), 1);
        assert_eq!(out.matches("- B").count(), 1);
    }

    #[test]
    fn caps_extreme_depth() {
        let mut texts = BTreeMap::new();
        let mut children: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for i in 0..1000 {
            texts.insert(i.to_string(), format!("n{i}"));
            children.insert(i.to_string(), vec![(i + 1).to_string()]);
        }
        texts.insert("1000".to_string(), "end".to_string());
        let out = render_tree("0", &texts, &children, 0, &mut BTreeSet::new());
        assert!(!out.contains("- end"));
    }

    #[test]
    fn tolerant_cells() {
        assert_eq!(tolerant_cell_text(&serde_json::json!("plain")), "plain");
        assert_eq!(tolerant_cell_text(&serde_json::json!(42)), "42");
        assert_eq!(tolerant_cell_text(&serde_json::json!(3.5)), "3.5");
        assert_eq!(tolerant_cell_text(&serde_json::json!(true)), "true");
        assert_eq!(
            tolerant_cell_text(&serde_json::json!([{"text": "a"}, {"insert": "b"}])),
            "a b"
        );
        assert_eq!(tolerant_cell_text(&serde_json::json!(null)), "");
    }
}

#[cfg(test)]
mod canvas_index_tests {
    use super::{canvas_mindmap_members, collect_canvas_elements, connector_endpoint_label};
    use serde_json::json;
    use std::collections::BTreeMap;

    #[test]
    fn collects_shapes_texts_and_connectors() {
        let elements = vec![
            json!({"id": "mm1", "type": "mindmap", "children": {"n1": {}, "n2": {}}}),
            json!({"id": "s1", "type": "shape", "shapeType": "rect", "text": "Box"}),
            json!({"id": "t1", "type": "text", "text": "Note"}),
            json!({"id": "c1", "type": "connector", "source": {"id": "s1"}, "target": {"id": "t1"}, "label": "uses"}),
            json!({"id": "x", "type": "group"}),
        ];
        let idx = collect_canvas_elements(&elements);
        assert_eq!(idx.mindmap_ids, vec!["mm1".to_string()]);
        assert_eq!(
            idx.shapes_by_id["s1"],
            ("rect".to_string(), "Box".to_string())
        );
        assert_eq!(idx.texts, vec!["Note".to_string()]);
        assert_eq!(
            idx.connectors[0],
            ("s1".to_string(), "t1".to_string(), "uses".to_string())
        );
        assert!(!idx.is_empty());
    }

    #[test]
    fn empty_canvas_index_is_empty() {
        assert!(collect_canvas_elements(&[]).is_empty());
    }

    #[test]
    fn mindmap_members_include_children_keys() {
        let elements = vec![json!({"id": "mm", "type": "mindmap", "children": {"a": {}, "b": {}}})];
        let map = canvas_mindmap_members(&elements);
        assert_eq!(
            map["mm"],
            vec!["mm".to_string(), "a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn connector_label_prefers_shape_text_then_canvas_text_then_id() {
        let mut shapes: BTreeMap<String, (String, String)> = BTreeMap::new();
        shapes.insert("s".to_string(), ("rect".to_string(), "Shape".to_string()));
        let mut texts: BTreeMap<&str, &str> = BTreeMap::new();
        texts.insert("t", "Canvas text");
        assert_eq!(connector_endpoint_label("s", &shapes, &texts), "Shape");
        assert_eq!(
            connector_endpoint_label("t", &shapes, &texts),
            "Canvas text"
        );
        // у шейпа пустой текст → фоллбек на текст канвы
        shapes.insert("e".to_string(), ("rect".to_string(), String::new()));
        texts.insert("e", "Fallback");
        assert_eq!(connector_endpoint_label("e", &shapes, &texts), "Fallback");
        assert_eq!(
            connector_endpoint_label("unknown", &shapes, &texts),
            "unknown"
        );
    }
}
