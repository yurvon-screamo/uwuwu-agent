//! Общий stateful wiremock-мок SiYuan-ядра для cmd-тестов (guard/cron/write_path,
//! дедуп мок-билдеров 20261107052603 — ревью PR #115: правило трёх). Один билдер:
//! REST-поверхность ядра + MCP-скелет (initialize → tools/call по action);
//! сценарий — данными [`MockScenario`], исходы — observers [`MockKernel`]
//! (журналы вызовов + render-состояние).
//!
//! Честность мока: render пагинируется как ядро — страница 1 несёт строки,
//! страница ≥2 пустая (терминация клиента); вставленная строка видна в render
//! по порогу «лага» (`row_visible_after_adds`); appendBlock применяет блок в
//! состояние дока и/или отвечает ошибкой — «потерянный ответ» и «честный сбой»
//! различимы. Ассерты тестов — по исходам (что куда записалось), не по
//! внутренним вызовам.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// Строка render в мок-ядре. Пустое поле `status` не даёт клетку — как
/// доска без колонки; `doc` — привязка
/// (block.id в Title-клетке).
#[derive(Clone)]
pub(crate) struct MockRow {
    pub(crate) item: String,
    pub(crate) title: String,
    pub(crate) status: String,
    pub(crate) doc: Option<String>,
}

/// Прямой ребёнок-док доски сценария: (docId, полный hpath, updated).
pub(crate) type BoardChild = (&'static str, &'static str, &'static str);

impl MockRow {
    pub(crate) fn new(item: &str, title: &str) -> Self {
        Self {
            item: item.to_string(),
            title: title.to_string(),
            status: String::new(),
            doc: None,
        }
    }

    pub(crate) fn with_status(mut self, status: &str) -> Self {
        self.status = status.to_string();
        self
    }

    pub(crate) fn with_doc(mut self, doc: &str) -> Self {
        self.doc = Some(doc.to_string());
        self
    }

    /// Строка render как JSON: Title-клетка всегда, статус — по заполненности.
    fn cells_json(&self) -> Value {
        let mut block = json!({ "content": self.title, "refSubtype": "d" });
        if let Some(doc) = &self.doc {
            block["id"] = json!(doc);
        }
        let mut cells = vec![json!({
            "value": { "keyID": "key-title", "type": "block", "block": block }
        })];
        if !self.status.is_empty() {
            cells.push(json!({
                "value": { "keyID": "key-status", "type": "mSelect",
                           "mSelect": [{ "content": self.status }] }
            }));
        }
        json!({ "id": self.item, "cells": cells })
    }
}

/// Доска сценария: docId → av и колонки database (id, name). hpath-маппинг
/// досок живёт в `MockScenario::hpath_ids`.
#[derive(Clone)]
pub(crate) struct MockBoard {
    pub(crate) doc_id: &'static str,
    pub(crate) av_id: &'static str,
    pub(crate) keys: Vec<(&'static str, &'static str)>,
}

/// Сценарий мок-ядра: статические данные поверх stateful-билдера. Дефолт —
/// одна доска uwuwu с полным набором колонок, вставка видна сразу.
/// `inserted_row` описывает строку вставки (title/status;
/// item берётся из очереди ответов, doc — из blockID аргумента item_add).
pub(crate) struct MockScenario {
    pub(crate) notebook: &'static str,
    pub(crate) boards: Vec<MockBoard>,
    /// hpath → docIds (getIDsByHPath): доски и тела-сироты.
    pub(crate) hpath_ids: Vec<(&'static str, Vec<&'static str>)>,
    /// Начальные строки render (общий пул для всех досок сценария).
    pub(crate) rows: Vec<MockRow>,
    /// Строка вставки видна в render, когда item_add-ов ≥ порога
    /// (usize::MAX — не видна вовсе: «лаг» ядра для verify-ретрая).
    pub(crate) row_visible_after_adds: usize,
    /// Спецификация строки вставки (см. struct doc).
    pub(crate) inserted_row: MockRow,
    /// Чужая строка с тем же тайтлом появляется после первой вставки
    /// (impedance для дедупа по факту render — «вторую строку не создаю»).
    pub(crate) impedance_row: Option<MockRow>,
    /// itemID-ответы item_add по порядку; последний повторяется;
    /// пусто — дефолт «item-mock».
    pub(crate) item_add_replies: Vec<&'static str>,
    /// appendBlock применяет блок в состояние дока (export это покажет).
    pub(crate) append_applies: bool,
    /// HTTP-статус appendBlock: 200; 500 — применённый блок при lost_response
    /// либо честный сбой (в паре с append_applies).
    pub(crate) append_status: u16,
    /// Состояние доков (docId → блоки): seed + применённые append-ы;
    /// export отдаёт join.
    pub(crate) doc_blocks: Vec<(&'static str, Vec<String>)>,
    /// Статические экспорты (docId → md) для доков без block-состояния.
    pub(crate) exports: Vec<(&'static str, String)>,
    /// Последовательные ответы block get_children; последний повторяется.
    pub(crate) get_children_seq: Vec<String>,
    /// block get: true — док жив (Type: NodeDocument, id эхом), false — not found.
    pub(crate) block_get_alive: bool,
    /// docId, существующие в SQL-индексе (пробы тел по id).
    pub(crate) sql_docs: Vec<&'static str>,
    /// FS-маршруты getFile: (path, status, body) до файлов воркспейса.
    pub(crate) file_routes: Vec<(&'static str, u16, &'static str)>,
    /// Файлы воркспейса (seed); putFile дописывает сюда же.
    pub(crate) files: Vec<(String, Vec<u8>)>,
    /// Путь, отвечающий «file does not exist» вопреки маршрутам
    /// (контрольная проба доски: файл «пропал»).
    pub(crate) missing_file_path: &'static str,
    /// item_remove снимает ВСЕ строки (симулятор runaway-удаления пост-чека).
    pub(crate) item_remove_drops_all: bool,
    /// Атрибуты блоков (docId → [(имя, значение)]) для getBlockAttrs;
    /// doc без записи → 404-конверт «block not found» (честная деградация).
    pub(crate) block_attrs: Vec<(&'static str, Vec<(String, String)>)>,
    /// Прямые дети-доки досок (проба board-orphans): hpath доски → дети
    /// (docId, полный hpath, updated). Отказ SQL детей — `sql_children_fail`.
    pub(crate) board_children: Vec<(&'static str, Vec<BoardChild>)>,
    /// true — SQL-запрос детей доски отвечает HTTP 500 (честный SKIP пробы).
    pub(crate) sql_children_fail: bool,
}

impl Default for MockScenario {
    fn default() -> Self {
        Self {
            notebook: "nb-1",
            boards: vec![MockBoard {
                doc_id: "board-doc",
                av_id: "av-1",
                keys: vec![
                    ("key-title", "Primary Key"),
                    ("key-status", "Status"),
                    ("key-branch", "Branch"),
                    ("key-pr", "PR"),
                ],
            }],
            hpath_ids: vec![("/uwuwu/Задачи uwuwu", vec!["board-doc"])],
            rows: Vec::new(),
            row_visible_after_adds: 1,
            inserted_row: MockRow::new("item-mock", "новая строка"),
            impedance_row: None,
            item_add_replies: Vec::new(),
            append_applies: false,
            append_status: 200,
            doc_blocks: Vec::new(),
            exports: Vec::new(),
            get_children_seq: vec![String::new()],
            block_get_alive: true,
            sql_docs: Vec::new(),
            file_routes: Vec::new(),
            files: Vec::new(),
            missing_file_path: "",
            item_remove_drops_all: false,
            block_attrs: Vec::new(),
            board_children: Vec::new(),
            sql_children_fail: false,
        }
    }
}

/// Stateful-мок ядра: сценарий + живое состояние (render-строки, доки, файлы)
/// + журналы вызовов (observers ассертов).
pub(crate) struct MockKernel {
    sc: MockScenario,
    rows: Mutex<Vec<MockRow>>,
    adds: AtomicUsize,
    impedance_shown: AtomicBool,
    replies: Mutex<Vec<String>>,
    children: Mutex<Vec<String>>,
    item_add_args: Mutex<Vec<Value>>,
    item_updates: Mutex<Vec<(String, String, String, Value)>>,
    item_removes: Mutex<Vec<String>>,
    created_docs: Mutex<Vec<(String, String)>>,
    appends: Mutex<Vec<String>>,
    deletes: Mutex<Vec<String>>,
    removed_docs: Mutex<Vec<String>>,
    put_files: Mutex<Vec<String>>,
    attr_writes: Mutex<Vec<(String, String, String)>>,
    doc_blocks: Mutex<HashMap<String, Vec<String>>>,
    files: Mutex<HashMap<String, Vec<u8>>>,
}

impl MockKernel {
    fn new(sc: MockScenario) -> Self {
        Self {
            replies: Mutex::new(sc.item_add_replies.iter().map(|s| s.to_string()).collect()),
            children: Mutex::new(sc.get_children_seq.clone()),
            doc_blocks: Mutex::new(
                sc.doc_blocks
                    .iter()
                    .map(|(doc, blocks)| (doc.to_string(), blocks.clone()))
                    .collect(),
            ),
            files: Mutex::new(sc.files.iter().cloned().collect()),
            rows: Mutex::new(sc.rows.clone()),
            sc,
            adds: AtomicUsize::new(0),
            impedance_shown: AtomicBool::new(false),
            item_add_args: Mutex::new(Vec::new()),
            item_updates: Mutex::new(Vec::new()),
            item_removes: Mutex::new(Vec::new()),
            created_docs: Mutex::new(Vec::new()),
            appends: Mutex::new(Vec::new()),
            deletes: Mutex::new(Vec::new()),
            removed_docs: Mutex::new(Vec::new()),
            put_files: Mutex::new(Vec::new()),
            attr_writes: Mutex::new(Vec::new()),
        }
    }

    /// Поднимает MockServer с полной поверхностью ядра по сценарию.
    pub(crate) async fn start(scenario: MockScenario) -> (Arc<Self>, MockServer) {
        let kernel = Arc::new(Self::new(scenario));
        let server = MockServer::start().await;
        kernel.mount(&server).await;
        (kernel, server)
    }

    // ——— observers: исходы для ассертов ———

    /// Строки render сейчас (состояние доски).
    pub(crate) fn rows(&self) -> Vec<MockRow> {
        self.rows.lock().unwrap().clone()
    }

    /// Снятие строки до прогона (предусловия сценария).
    pub(crate) fn remove_row(&self, item: &str) {
        self.rows.lock().unwrap().retain(|r| r.item != item);
    }

    pub(crate) fn item_add_count(&self) -> usize {
        self.adds.load(Ordering::SeqCst)
    }

    /// Аргументы item_add как есть (blockID/detached различимы).
    pub(crate) fn item_add_args(&self) -> Vec<Value> {
        self.item_add_args.lock().unwrap().clone()
    }

    /// (av, keyID, itemID, value) всех item_update.
    pub(crate) fn item_updates(&self) -> Vec<(String, String, String, Value)> {
        self.item_updates.lock().unwrap().clone()
    }

    pub(crate) fn item_removes(&self) -> Vec<String> {
        self.item_removes.lock().unwrap().clone()
    }

    /// (hpath, markdown) всех createDocWithMd.
    pub(crate) fn created_docs(&self) -> Vec<(String, String)> {
        self.created_docs.lock().unwrap().clone()
    }

    /// Тела всех appendBlock (raw).
    pub(crate) fn appends(&self) -> Vec<String> {
        self.appends.lock().unwrap().clone()
    }

    /// id всех deleteBlock.
    pub(crate) fn deletes(&self) -> Vec<String> {
        self.deletes.lock().unwrap().clone()
    }

    /// docId всех removeDocByID (тела в трэш).
    pub(crate) fn removed_docs(&self) -> Vec<String> {
        self.removed_docs.lock().unwrap().clone()
    }

    /// workspace-пути всех putFile (undo-журналы).
    pub(crate) fn put_files(&self) -> Vec<String> {
        self.put_files.lock().unwrap().clone()
    }

    /// (blockId, имя атрибута, значение) всех setBlockAttrs.
    pub(crate) fn attr_writes(&self) -> Vec<(String, String, String)> {
        self.attr_writes.lock().unwrap().clone()
    }

    /// Состояние дока (применённые блоки) для ассертов comment-флоу.
    pub(crate) fn doc_blocks_of(&self, doc: &str) -> Vec<String> {
        self.doc_blocks
            .lock()
            .unwrap()
            .get(doc)
            .cloned()
            .unwrap_or_default()
    }

    // ——— монтирование поверхности ———

    async fn mount(self: &Arc<Self>, server: &MockServer) {
        // REST: ноутбуки
        Mock::given(method("POST"))
            .and(path("/api/notebook/lsNotebooks"))
            .respond_with({
                let nb = self.sc.notebook;
                move |_: &Request| {
                    rest_ok(
                        json!({ "notebooks": [{ "id": nb, "name": "Projects", "closed": false }] }),
                    )
                }
            })
            .mount(server)
            .await;

        // REST: SQL — av-носитель доски (по root_id) и пробы доков по id
        Mock::given(method("POST"))
            .and(path("/api/query/sql"))
            .respond_with({
                let kernel = Arc::clone(self);
                move |req: &Request| {
                    let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
                    let stmt = body.get("stmt").and_then(|s| s.as_str()).unwrap_or("");
                    let data = kernel.sql_data(stmt);
                    if data.get("__sql_fail__").is_some() {
                        return ResponseTemplate::new(500).set_body_string(
                            r#"{"code":500,"msg":"metadata is not ready","data":[]}"#,
                        );
                    }
                    rest_ok(data)
                }
            })
            .mount(server)
            .await;

        // REST: docId по hpath (доски и тела-сироты — статическая карта сценария)
        Mock::given(method("POST"))
            .and(path("/api/filetree/getIDsByHPath"))
            .respond_with({
                let kernel = Arc::clone(self);
                move |req: &Request| {
                    let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
                    let hpath = body.get("path").and_then(|p| p.as_str()).unwrap_or("");
                    let ids = kernel
                        .sc
                        .hpath_ids
                        .iter()
                        .find(|(p, _)| *p == hpath)
                        .map(|(_, ids)| ids.clone())
                        .unwrap_or_default();
                    rest_ok(json!(ids))
                }
            })
            .mount(server)
            .await;

        // REST: тело → док (запись в журнал вызовов; docId фиксированный —
        // сценарии ассертят hpath/markdown, не docId)
        Mock::given(method("POST"))
            .and(path("/api/filetree/createDocWithMd"))
            .respond_with({
                let kernel = Arc::clone(self);
                move |req: &Request| {
                    let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
                    let hpath = body
                        .get("path")
                        .and_then(|p| p.as_str())
                        .unwrap_or("")
                        .to_string();
                    let md = body
                        .get("markdown")
                        .and_then(|m| m.as_str())
                        .unwrap_or("")
                        .to_string();
                    kernel.created_docs.lock().unwrap().push((hpath, md));
                    rest_ok(json!("doc-new"))
                }
            })
            .mount(server)
            .await;

        // REST: экспорт дока — block-состояние (применённые append-ы) или
        // статическая карта сценария
        Mock::given(method("POST"))
            .and(path("/api/export/exportMdContent"))
            .respond_with({
                let kernel = Arc::clone(self);
                move |req: &Request| {
                    let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
                    let id = body.get("id").and_then(|i| i.as_str()).unwrap_or("");
                    let content = {
                        let blocks = kernel.doc_blocks.lock().unwrap();
                        match blocks.get(id) {
                            Some(v) => v.join("\n"),
                            None => kernel
                                .sc
                                .exports
                                .iter()
                                .find(|(doc, _)| *doc == id)
                                .map(|(_, c)| c.clone())
                                .unwrap_or_default(),
                        }
                    };
                    rest_ok(json!({ "hPath": "", "content": content }))
                }
            })
            .mount(server)
            .await;

        // REST: appendBlock — запись тела, применение в состояние дока и/или ошибка
        Mock::given(method("POST"))
            .and(path("/api/block/appendBlock"))
            .respond_with({
                let kernel = Arc::clone(self);
                move |req: &Request| {
                    let raw = String::from_utf8_lossy(&req.body).to_string();
                    let parsed: Value = serde_json::from_str(&raw).unwrap_or_default();
                    kernel.appends.lock().unwrap().push(raw);
                    if kernel.sc.append_applies {
                        let doc = parsed
                            .get("parentID")
                            .and_then(|i| i.as_str())
                            .unwrap_or("")
                            .to_string();
                        let md = parsed
                            .get("data")
                            .and_then(|d| d.as_str())
                            .unwrap_or("")
                            .to_string();
                        kernel
                            .doc_blocks
                            .lock()
                            .unwrap()
                            .entry(doc)
                            .or_default()
                            .push(md);
                    }
                    if kernel.sc.append_status == 200 {
                        rest_ok(json!(null))
                    } else {
                        ResponseTemplate::new(kernel.sc.append_status)
                    }
                }
            })
            .mount(server)
            .await;

        // REST: удаление блока (overwrite-волны body --replace)
        Mock::given(method("POST"))
            .and(path("/api/block/deleteBlock"))
            .respond_with({
                let kernel = Arc::clone(self);
                move |req: &Request| {
                    let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
                    let id = body
                        .get("id")
                        .and_then(|i| i.as_str())
                        .unwrap_or("")
                        .to_string();
                    kernel.deletes.lock().unwrap().push(id);
                    rest_ok(json!(null))
                }
            })
            .mount(server)
            .await;

        // REST: putFile — write-ahead журналы в файлы воркспейса
        Mock::given(method("POST"))
            .and(path("/api/file/putFile"))
            .respond_with({
                let kernel = Arc::clone(self);
                move |req: &Request| {
                    let raw = String::from_utf8_lossy(&req.body).to_string();
                    if let Some(p) = multipart_path(&raw) {
                        kernel.put_files.lock().unwrap().push(p.clone());
                        kernel.files.lock().unwrap().insert(p, req.body.clone());
                    }
                    ResponseTemplate::new(200).set_body_string(r#"{"code":0,"msg":"","data":null}"#)
                }
            })
            .mount(server)
            .await;

        // REST: getFile — маршруты сценария, файлы воркспейса, 404-конверт
        Mock::given(method("POST"))
            .and(path("/api/file/getFile"))
            .respond_with({
                let kernel = Arc::clone(self);
                move |req: &Request| {
                    let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
                    let file = body
                        .get("path")
                        .and_then(|p| p.as_str())
                        .unwrap_or("")
                        .to_string();
                    if !kernel.sc.missing_file_path.is_empty()
                        && file == kernel.sc.missing_file_path
                    {
                        return missing_file();
                    }
                    for (route, status, body) in &kernel.sc.file_routes {
                        if file == *route {
                            return ResponseTemplate::new(*status).set_body_string(*body);
                        }
                    }
                    let seeded = kernel.files.lock().unwrap().get(&file).cloned();
                    match seeded {
                        Some(bytes) => ResponseTemplate::new(200).set_body_bytes(bytes),
                        None => missing_file(),
                    }
                }
            })
            .mount(server)
            .await;

        // REST: трэш тел (removeDocByID)
        Mock::given(method("POST"))
            .and(path("/api/filetree/removeDocByID"))
            .respond_with({
                let kernel = Arc::clone(self);
                move |req: &Request| {
                    let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
                    let id = body
                        .get("id")
                        .and_then(|i| i.as_str())
                        .unwrap_or("")
                        .to_string();
                    kernel.removed_docs.lock().unwrap().push(id);
                    ResponseTemplate::new(200).set_body_string(r#"{"code":0,"msg":"","data":null}"#)
                }
            })
            .mount(server)
            .await;

        // REST: атрибуты блоков — чтение (custom-schedule крон-мастеров);
        // блок без записи в сценарии → 404-конверт «block not found»
        Mock::given(method("POST"))
            .and(path("/api/attr/getBlockAttrs"))
            .respond_with({
                let kernel = Arc::clone(self);
                move |req: &Request| {
                    let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
                    let id = body.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    let attrs = kernel
                        .sc
                        .block_attrs
                        .iter()
                        .find(|(doc, _)| *doc == id)
                        .map(|(_, a)| a.clone());
                    match attrs {
                        Some(attrs) => {
                            let map: serde_json::Map<String, Value> =
                                attrs.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
                            rest_ok(Value::Object(map))
                        }
                        None => ResponseTemplate::new(200).set_body_string(
                            json!({ "code": -1, "msg": "block not found" }).to_string(),
                        ),
                    }
                }
            })
            .mount(server)
            .await;

        // REST: запись атрибутов — ровно первая пара attrs-объекта (наблюдатель)
        Mock::given(method("POST"))
            .and(path("/api/attr/setBlockAttrs"))
            .respond_with({
                let kernel = Arc::clone(self);
                move |req: &Request| {
                    let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
                    let id = body
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let attrs = body.get("attrs").cloned().unwrap_or_default();
                    if let Some((name, value)) =
                        attrs.as_object().and_then(|m| m.into_iter().next())
                    {
                        kernel.attr_writes.lock().unwrap().push((
                            id,
                            name.clone(),
                            value.as_str().unwrap_or("").to_string(),
                        ));
                    }
                    ResponseTemplate::new(200)
                        .set_body_string(json!({ "code": 0, "msg": "", "data": null }).to_string())
                }
            })
            .mount(server)
            .await;

        // MCP: initialize → notifications → tools/call по action
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with({
                let kernel = Arc::clone(self);
                move |req: &Request| {
                    let body = String::from_utf8_lossy(&req.body);
                    if body.contains("\"initialize\"") {
                        return mcp_init();
                    }
                    if body.contains("notifications/") {
                        return ResponseTemplate::new(200);
                    }
                    if !body.contains("tools/call") {
                        return ResponseTemplate::new(200).set_body_string(
                            r#"{"jsonrpc":"2.0","error":{"code":-1,"message":"unexpected"}}"#,
                        );
                    }
                    let parsed: Value = serde_json::from_slice(&req.body).unwrap_or_default();
                    let args = parsed
                        .pointer("/params/arguments")
                        .cloned()
                        .unwrap_or_default();
                    let action = args
                        .get("action")
                        .and_then(|a| a.as_str())
                        .unwrap_or("")
                        .to_string();
                    let id = args
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    match action.as_str() {
                        "render" => kernel.handle_render(&args),
                        "keys" => kernel.handle_keys(&id),
                        "item_add" => kernel.handle_item_add(&args),
                        "item_update" => kernel.handle_item_update(&args, &id),
                        "item_remove" => kernel.handle_item_remove(&args),
                        "get" => kernel.handle_block_get(&id),
                        "get_children" => kernel.handle_get_children(),
                        _ => mcp_text(json!({ "action": "unknown", "data": null })),
                    }
                }
            })
            .mount(server)
            .await;
    }

    // ——— MCP-действия над состоянием ———

    fn sql_data(&self, stmt: &str) -> Value {
        // проба board-orphans: уникальная сигнатура stmt («select id,
        // hpath, ial from blocks») — чужие SQL с hpath like не попадают
        if stmt.contains("select id, hpath, ial from blocks") {
            if self.sc.sql_children_fail {
                // синтаксически валидный stmt — отказ ядра как HTTP 500
                return json!({ "__sql_fail__": true });
            }
            if let Some((_, children)) = self
                .sc
                .board_children
                .iter()
                .find(|(hpath, _)| stmt.contains(&format!("hpath like '{hpath}/%'")))
            {
                let rows: Vec<Value> = children
                    .iter()
                    .map(|(id, child_hpath, updated)| {
                        json!({
                            "id": id,
                            "hpath": child_hpath,
                            "ial": format!(
                                "{{: type=\"doc\" updated=\"{updated}\" id=\"{id}\" title=\"\"}}"
                            ),
                        })
                    })
                    .collect();
                return json!(rows);
            }
            return json!([]);
        }
        // дискавери досок задач (глобальный резолв без --project): hpath
        // сценария фильтруются той же конвенцией, что парсит резолв
        if stmt.contains("content like 'Задачи %'") {
            let mut hits: Vec<Value> = self
                .sc
                .hpath_ids
                .iter()
                .filter_map(|(hpath, ids)| {
                    crate::siyuan_task::task_board_parts(hpath)
                        .and_then(|_| ids.first().map(|id| json!({ "id": id, "hpath": hpath })))
                })
                .collect();
            hits.sort_by(|a, b| {
                let pa = a.get("hpath").and_then(|h| h.as_str()).unwrap_or("");
                let pb = b.get("hpath").and_then(|h| h.as_str()).unwrap_or("");
                pa.cmp(pb)
            });
            return json!(hits);
        }
        if let Some(root) = stmt
            .split("root_id='")
            .nth(1)
            .map(|s| s.split('\'').next().unwrap_or(""))
        {
            // av-носитель доски по root_id
            return match self.sc.boards.iter().find(|b| b.doc_id == root) {
                Some(b) => json!([{
                    "id": format!("block-{}", b.doc_id),
                    "markdown": format!(
                        "<div data-type=\"NodeAttributeView\" data-av-id=\"{}\"></div>",
                        b.av_id
                    )
                }]),
                None => json!([]),
            };
        }
        if stmt.contains("type='av'") {
            // запрос av-носителя без root: единственная доска сценария
            let b = &self.sc.boards[0];
            return json!([{
                "id": format!("block-{}", b.doc_id),
                "markdown": format!(
                    "<div data-type=\"NodeAttributeView\" data-av-id=\"{}\"></div>",
                    b.av_id
                )
            }]);
        }
        // пробы существования доков по id (`id='...'`)
        let ids: Vec<&str> = stmt
            .split("id='")
            .skip(1)
            .map(|rest| rest.split('\'').next().unwrap_or(""))
            .filter(|id| !id.is_empty())
            .collect();
        let hits: Vec<Value> = ids
            .iter()
            .filter(|id| self.sc.sql_docs.iter().any(|d| d == *id))
            .map(|id| json!({ "id": id }))
            .collect();
        json!(hits)
    }

    /// render: колонки по av-доске, строки — общий пул сценария (мультидосочные
    /// сценарии с разными строками не моделируются); пагинация честная —
    /// страница 1 несёт строки, страница ≥2 пустая (терминация клиента).
    fn handle_render(&self, args: &Value) -> ResponseTemplate {
        let av = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let page = args.get("page").and_then(|p| p.as_u64()).unwrap_or(1);
        let view_rows: Vec<Value> = if page <= 1 {
            self.rows
                .lock()
                .unwrap()
                .iter()
                .map(MockRow::cells_json)
                .collect()
        } else {
            Vec::new()
        };
        let count = view_rows.len();
        let columns: Vec<Value> = self
            .sc
            .boards
            .iter()
            .find(|b| b.av_id == av)
            .map(|b| {
                b.keys
                    .iter()
                    .map(|(id, name)| json!({ "id": id, "name": name }))
                    .collect()
            })
            .unwrap_or_default();
        mcp_text(json!({
            "action": "render",
            "data": { "view": { "columns": columns, "rowCount": count, "rows": view_rows } }
        }))
    }

    fn handle_keys(&self, av: &str) -> ResponseTemplate {
        // неизвестный av — пустой ответ без тихого fallback на чужую доску
        let arr: Vec<Value> = self
            .sc
            .boards
            .iter()
            .find(|b| b.av_id == av)
            .map(|b| {
                b.keys
                    .iter()
                    .map(|(id, name)| json!({ "id": id, "name": name }))
                    .collect()
            })
            .unwrap_or_default();
        mcp_text(json!({ "action": "keys", "data": { "keys": arr } }))
    }

    fn next_item(&self) -> String {
        let mut q = self.replies.lock().unwrap();
        if q.len() > 1 {
            q.remove(0)
        } else {
            q.last().cloned().unwrap_or_else(|| "item-mock".to_string())
        }
    }

    fn handle_item_add(&self, args: &Value) -> ResponseTemplate {
        self.adds.fetch_add(1, Ordering::SeqCst);
        self.item_add_args.lock().unwrap().push(args.clone());
        let item = self.next_item();
        let adds = self.adds.load(Ordering::SeqCst);
        let mut rows = self.rows.lock().unwrap();
        if let Some(impedance) = self.sc.impedance_row.clone() {
            // чужая строка появляется с первой вставки и остаётся
            if adds >= 1 && !self.impedance_shown.swap(true, Ordering::SeqCst) {
                rows.push(impedance);
            }
        }
        if adds >= self.sc.row_visible_after_adds {
            // detached-вставки несут тайтл в content; bound-вставки —
            // тайтл из block-контента (сценарий inserted_row)
            let title = args
                .get("content")
                .and_then(|c| c.as_str())
                .unwrap_or(&self.sc.inserted_row.title)
                .to_string();
            rows.push(MockRow {
                doc: args
                    .get("blockID")
                    .and_then(|b| b.as_str())
                    .map(String::from),
                item: item.clone(),
                title,
                status: self.sc.inserted_row.status.clone(),
            });
        }
        mcp_text(json!({ "action": "item_add", "data": { "itemID": item } }))
    }

    fn handle_item_update(&self, args: &Value, av: &str) -> ResponseTemplate {
        let key = args
            .get("keyID")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let item = args
            .get("itemID")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let value = args.get("value").cloned().unwrap_or(Value::Null);
        self.item_updates
            .lock()
            .unwrap()
            .push((av.to_string(), key, item, value));
        mcp_text(json!({ "action": "item_update", "data": null }))
    }

    fn handle_item_remove(&self, args: &Value) -> ResponseTemplate {
        let ids: Vec<String> = args
            .get("itemIDs")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|i| i.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        {
            let mut rows = self.rows.lock().unwrap();
            if self.sc.item_remove_drops_all {
                rows.clear();
            } else {
                rows.retain(|r| !ids.contains(&r.item));
            }
        }
        self.item_removes.lock().unwrap().extend(ids);
        mcp_text(json!({ "action": "item_remove", "data": null }))
    }

    fn handle_block_get(&self, id: &str) -> ResponseTemplate {
        let text = if self.sc.block_get_alive {
            format!("ID: {id}\nType: NodeDocument\nHPath: /док/{id}")
        } else {
            "not found".to_string()
        };
        mcp_raw(text)
    }

    fn handle_get_children(&self) -> ResponseTemplate {
        let mut seq = self.children.lock().unwrap();
        let text = if seq.len() > 1 {
            seq.remove(0)
        } else {
            seq.last().cloned().unwrap_or_default()
        };
        mcp_raw(text)
    }
}

/// Env-переменные клиента на мок-сервер (serial-тесты: env мутабелен).
pub(crate) fn set_env(server: &MockServer) {
    std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
    std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");
}

/// kernel-конверт OK с data.
fn rest_ok(data: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_string(json!({ "code": 0, "data": data }).to_string())
}

/// MCP-ответ tools/call, где content[0].text — сериализованный payload.
fn mcp_text(payload: Value) -> ResponseTemplate {
    mcp_raw(payload.to_string())
}

/// MCP-ответ tools/call, где content[0].text — сырой текст (block get / get_children).
fn mcp_raw(text: String) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_string(
        json!({
            "jsonrpc": "2.0", "id": 2,
            "result": { "content": [{ "type": "text", "text": text }] }
        })
        .to_string(),
    )
}

fn mcp_init() -> ResponseTemplate {
    ResponseTemplate::new(200)
        .set_body_string(
            r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"mock","version":"3"}}}"#,
        )
        .append_header("mcp-session-id", "sess-mock")
}

/// Отсутствующий файл: контракт ядра 3.8.9 — 202 + 404-конверт.
fn missing_file() -> ResponseTemplate {
    ResponseTemplate::new(202).set_body_string(r#"{"code":404,"msg":"file does not exist"}"#)
}

/// workspace-путь из multipart-тела putFile (поле path).
fn multipart_path(body: &str) -> Option<String> {
    const MARK: &str = "name=\"path\"\r\n\r\n";
    let idx = body.find(MARK)?;
    let rest = &body[idx + MARK.len()..];
    rest.split("\r\n").next().map(String::from)
}
