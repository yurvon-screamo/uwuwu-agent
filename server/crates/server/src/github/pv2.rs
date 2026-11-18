//! Pv2 REST-поверхность (#1236): мутации статусов и их пре-чтения —
//! REST ProjectsV2 вместо GraphQL (развод независимых бакетов квот,
//! инстанс 16.11 #1156/#378). Прецедент — fields.rs; классификация
//! отказов — общий gherr, тег «gh rest [rate_limited]» для verify.
//! Контракты — живая проба среза 0 (тело задачи #1236).
//! Один когезивный транспорт (>200 строк): парсеры вынесены
//! (pv2_parse.rs), дальнейший разрез выносит HTTP-контекст мутаций
//! из кеш-политик без отдельной причины изменения.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uwuwu_usecase::{UsecaseError, UsecaseResult};

use super::hub::TtlCache;
use super::pv2_parse::{next_after, parse_field_catalog, parse_item_entry, parse_status_value};

/// Мета доски для REST-мутаций: номер проекта (путь REST), numeric id
/// Status-поля, карта «имя опции → hex-идентификатор» (проба среза 0).
pub(crate) struct Pv2Meta {
    pub(crate) project_number: u64,
    pub(crate) status_field_id: u64,
    pub(crate) options: HashMap<String, String>,
}

/// Ответ REST с заголовком пагинации: list items ходит по Link-курсору.
struct RestResponse {
    status: reqwest::StatusCode,
    link: Option<String>,
    body: Value,
}

pub(crate) struct Pv2Rest {
    repo: String,
    token: String,
    http: reqwest::Client,
    /// Мета стабильна (номер проекта/поле/опции) — кеш без TTL,
    /// сброс только при громком отказе носителя.
    meta: Mutex<Option<Arc<Pv2Meta>>>,
    /// issue number → Pv2 item id: list items пагинацией; TTL и
    /// refresh-on-miss — строки добавляются/сносятся вне сервера.
    items: TtlCache<HashMap<u64, u64>>,
    /// write-through статусов строк: read-лаг Projects v2 минутного
    /// масштаба (проба среза 0) — повторный гард edit() в TTL-окне
    /// читает собственную запись, а не стэйл носителя.
    status_wt: TtlCache<String>,
}

const ITEM_PAGES: usize = 10;
const ITEMS_TTL: std::time::Duration = std::time::Duration::from_secs(240);
const STATUS_TTL: std::time::Duration = std::time::Duration::from_secs(60);

impl Pv2Rest {
    pub(crate) fn new(repo: String, token: String) -> Self {
        Self {
            repo,
            token,
            http: reqwest::Client::builder()
                .user_agent(concat!("uwuwu-server/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("reqwest client"),
            meta: Mutex::new(None),
            items: TtlCache::new(ITEMS_TTL),
            status_wt: TtlCache::new(STATUS_TTL),
        }
    }

    fn owner(&self) -> UsecaseResult<&str> {
        self.repo
            .split_once('/')
            .map(|(owner, _)| owner)
            .ok_or_else(|| UsecaseError::Board(format!("repo: {}", self.repo)))
    }

    async fn rest(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> UsecaseResult<RestResponse> {
        let mut req = self
            .http
            .request(method, format!("https://api.github.com{path}"))
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json");
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| UsecaseError::Board(format!("pv2 rest transport {path}: {e}")))?;
        let status = resp.status();
        let link = resp
            .headers()
            .get(reqwest::header::LINK)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let headers = resp.headers().clone();
        let text = resp
            .text()
            .await
            .map_err(|e| UsecaseError::Board(format!("pv2 rest body {path}: {e}")))?;
        if let Some(fail) = crate::gherr::classify(status, &headers) {
            let kind = match fail {
                crate::gherr::GhFail::Permission => "PERMISSION",
                crate::gherr::GhFail::Core { .. } | crate::gherr::GhFail::Secondary { .. } => {
                    "RATE_LIMITED"
                }
            };
            return Err(UsecaseError::Board(format!(
                "gh rest [{kind}]: {path}: {fail}"
            )));
        }
        let parsed = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::Null)
        };
        if !status.is_success() {
            return Err(UsecaseError::Board(format!(
                "gh api {path}: HTTP {status}: {}",
                text.chars().take(140).collect::<String>()
            )));
        }
        Ok(RestResponse {
            status,
            link,
            body: parsed,
        })
    }

    /// Мета доски: резолв номера проекта по тайтлу + каталог Status-поля.
    pub(crate) async fn meta(&self, board: &str) -> UsecaseResult<Arc<Pv2Meta>> {
        if let Some(hit) = self.meta.lock().expect("pv2 meta lock").clone() {
            return Ok(hit);
        }
        let owner = self.owner()?;
        let projects = self
            .rest(
                reqwest::Method::GET,
                &format!("/users/{owner}/projectsV2?per_page=100"),
                None,
            )
            .await?;
        let number = projects
            .body
            .as_array()
            .and_then(|list| {
                list.iter()
                    .find(|project| project["title"] == board)
                    .and_then(|project| project["number"].as_u64())
            })
            .ok_or_else(|| {
                UsecaseError::Board(format!("проект «{board}» в projectsV2 {owner} не найден"))
            })?;
        let fields = self
            .rest(
                reqwest::Method::GET,
                &format!("/users/{owner}/projectsV2/{number}/fields?per_page=100"),
                None,
            )
            .await?;
        let meta = Arc::new(parse_field_catalog(number, &fields.body, board)?);
        *self.meta.lock().expect("pv2 meta lock") = Some(meta.clone());
        Ok(meta)
    }

    /// item id строки: живой кеш → рефреш индекса → мисса (не на доске).
    pub(crate) async fn item_id(&self, board: &str, number: u64) -> UsecaseResult<Option<u64>> {
        let now = std::time::Instant::now();
        if let Some(hit) = self.items.get(board, now) {
            if let Some(id) = hit.get(&number) {
                return Ok(Some(*id));
            }
        }
        let map = self.fetch_items(board).await?;
        let arc = self.items.put(board, map, now);
        Ok(arc.get(&number).copied())
    }

    async fn fetch_items(&self, board: &str) -> UsecaseResult<HashMap<u64, u64>> {
        let meta = self.meta(board).await?;
        let owner = self.owner()?;
        let mut map = HashMap::new();
        let mut url = format!(
            "/users/{owner}/projectsV2/{}/items?per_page=100",
            meta.project_number
        );
        for _ in 0..ITEM_PAGES {
            let resp = self.rest(reqwest::Method::GET, &url, None).await?;
            if let Some(list) = resp.body.as_array() {
                for item in list {
                    if let Some((number, id)) = parse_item_entry(item) {
                        map.insert(number, id);
                    }
                }
            }
            match next_after(resp.link.as_deref()) {
                Some(after) => {
                    url = format!(
                        "/users/{owner}/projectsV2/{}/items?per_page=100&after={after}",
                        meta.project_number
                    );
                }
                None => break,
            }
        }
        Ok(map)
    }

    /// Чтение НОСИТЕЛЯ (GET item, мимо write-through — verify обязан
    /// подтверждать GitHub, не собственную запись): None — строки нет
    /// (мисса индекса или 404); Some("") — Status не читается (не задан
    /// или read-лаг Projects v2, проба среза 0).
    pub(crate) async fn fetch_status(
        &self,
        board: &str,
        number: u64,
    ) -> UsecaseResult<Option<String>> {
        let meta = self.meta(board).await?;
        let owner = self.owner()?;
        let Some(item) = self.item_id(board, number).await? else {
            return Ok(None);
        };
        let resp = self
            .rest(
                reqwest::Method::GET,
                &format!(
                    "/users/{owner}/projectsV2/{}/items/{item}",
                    meta.project_number
                ),
                None,
            )
            .await?;
        if resp.status == reqwest::StatusCode::NOT_FOUND {
            // индекс застал (строку снесли) — сброс, следующий рефреш
            self.items.invalidate(board);
            return Ok(None);
        }
        Ok(Some(
            parse_status_value(&resp.body["fields"]).unwrap_or_default(),
        ))
    }

    /// Raw-имя Status строки на доске для гарда: None — строки нет;
    /// Some("") — есть, но Status не читается (контракт прежнего
    /// carrier_status). write-through обслуживает окно read-лага.
    pub(crate) async fn read_status(
        &self,
        board: &str,
        number: u64,
    ) -> UsecaseResult<Option<String>> {
        let key = format!("{board}#{number}");
        let now = std::time::Instant::now();
        if let Some(hit) = self.status_wt.get(&key, now) {
            return Ok(Some(hit.as_str().to_string()));
        }
        self.fetch_status(board, number).await
    }

    /// PATCH Status + write-through. Возврат — raw-имя Status из ответа
    /// мутации (None — поле не отдалось, read-лаг): материал verify;
    /// write-through ставится до verify — мутация принята GitHub.
    pub(crate) async fn set_status(
        &self,
        board: &str,
        number: u64,
        status_name: &str,
    ) -> UsecaseResult<Option<String>> {
        let meta = self.meta(board).await?;
        let option = meta.options.get(status_name).cloned().ok_or_else(|| {
            UsecaseError::Board(format!("Status-опция «{status_name}» не найдена"))
        })?;
        let owner = self.owner()?;
        let Some(item) = self.item_id(board, number).await? else {
            return Err(UsecaseError::Board(format!(
                "строка #{number} не на доске «{board}»"
            )));
        };
        let resp = self
            .rest(
                reqwest::Method::PATCH,
                &format!(
                    "/users/{owner}/projectsV2/{}/items/{item}",
                    meta.project_number
                ),
                Some(json!({ "fields": [
                    { "id": meta.status_field_id, "value": option }
                ] })),
            )
            .await?;
        if !resp.status.is_success() {
            return Err(UsecaseError::Board(format!(
                "patch status #{number}: не применилось (HTTP {})",
                resp.status
            )));
        }
        let key = format!("{board}#{number}");
        self.status_wt
            .put(&key, status_name.to_string(), std::time::Instant::now());
        Ok(parse_status_value(&resp.body["fields"]))
    }

    /// POST add строки на доску (по issue number, node_id не нужен) +
    /// инкремент живого индекса. Возврат — numeric item id.
    pub(crate) async fn add_item(&self, board: &str, number: u64) -> UsecaseResult<u64> {
        let meta = self.meta(board).await?;
        let owner = self.owner()?;
        let (repo_owner, repo_name) = self.repo.split_once('/').unwrap_or(("", ""));
        let resp = self
            .rest(
                reqwest::Method::POST,
                &format!("/users/{owner}/projectsV2/{}/items", meta.project_number),
                Some(json!({
                    "type": "Issue",
                    "owner": repo_owner,
                    "repo": repo_name,
                    "number": number
                })),
            )
            .await?;
        if !resp.status.is_success() {
            return Err(UsecaseError::Board(format!(
                "add #{number}: не применилось (HTTP {})",
                resp.status
            )));
        }
        let id = resp
            .body
            .as_object()
            .and_then(|item| item.get("id"))
            .and_then(Value::as_u64)
            .ok_or_else(|| UsecaseError::Board(format!("add #{number}: нет item id")))?;
        let now = std::time::Instant::now();
        if let Some(hit) = self.items.get(board, now) {
            let mut map = (*hit).clone();
            map.insert(number, id);
            self.items.put(board, map, now);
        }
        Ok(id)
    }
}
