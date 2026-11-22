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
use std::sync::Arc;
use uwuwu_domain::Status;
use uwuwu_usecase::{status_names, UsecaseError, UsecaseResult};

use super::hub::TtlCache;
use super::pv2_parse::{next_after, parse_field_catalog, parse_item_entry, parse_status_value};

/// Мета доски для REST-мутаций: номер проекта (путь REST), numeric id
/// Status-поля, карта «имя опции → hex-идентификатор» (проба среза 0).
pub(crate) struct Pv2Meta {
    pub(crate) project_number: u64,
    pub(crate) status_field_id: u64,
    pub(crate) options: HashMap<String, String>,
}

/// Резолв option-id Status по имени: канон или легаси (контракт окна
/// merge→rename #1383 — доска может нести русские опции при английском
/// каноне; lookup идёт по обоим, канон первым). Опция никогда не
/// создаётся: авто-создание дало бы дубль-опцию и расщепление строк.
pub(crate) fn resolve_status_option<'a>(
    options: &'a HashMap<String, String>,
    status_name: &str,
) -> Option<&'a String> {
    let status = uwuwu_usecase::parse_status(status_name);
    if status != Status::Empty {
        if let Some(id) = status_names(status).iter().find_map(|n| options.get(*n)) {
            return Some(id);
        }
    }
    options.get(status_name)
}

/// Строка индекса: numeric item id + последнее прочитанное Status-значение
/// носителя (из list items с fields-параметром).
#[derive(Clone)]
pub(crate) struct Pv2Item {
    pub(crate) id: u64,
    pub(crate) status: Option<String>,
}

/// Ответ REST: не-2xx отдаётся наверх со статусом — толерантные ветки
/// (404 снесённой строки) разбирают сами (прецедент hub.rest/fields.rest);
/// Err — только transport/gherr-классы (403/429).
struct RestResponse {
    status: reqwest::StatusCode,
    link: Option<String>,
    body: Value,
}

pub(crate) struct Pv2Rest {
    repo: String,
    token: String,
    http: reqwest::Client,
    /// Мета по имени доски (мультиборд: один GitHubHub обслуживает все
    /// доски конфига — ключирование обязательно, ревью 1 High-1 #1236).
    /// TTL 10 мин: хирургия владельца над Status-полем доски
    /// восстанавливает мутации без рестарта сервера.
    meta: TtlCache<Pv2Meta>,
    /// issue number → Pv2 item (numeric id + Status-значение носителя):
    /// list items пагинацией с fields=<Status field id> — GET items/{id}
    /// single-select Status не отдаёт никогда (тест-раунд #1236); TTL и
    /// refresh-on-miss — строки добавляются/сносятся вне сервера.
    items: TtlCache<HashMap<u64, Pv2Item>>,
    /// write-through статусов строк: read-лаг Projects v2 минутного
    /// масштаба (проба среза 0) — повторный гард edit() в TTL-окне
    /// читает собственную запись, а не стэйл носителя.
    status_wt: TtlCache<String>,
}

const ITEM_PAGES: usize = 10;
/// TTL items-индекса доски (карта целиком: item_entry при TTL-смерти
/// или миссе любого number заменяет её через fetch_items+put —
/// до-записные статусы строк возвращаются лаг-фетчами носителя).
/// Связка с STATUS_TTL — инвариант #1346, см. ниже.
const ITEMS_TTL: std::time::Duration = std::time::Duration::from_secs(240);
/// Write-through статусов строк (read-лаг Projects v2 минутного
/// масштаба — проба среза 0 #1236; инцидент #1345: гард в окне
/// 60–240 с от записи читал до-записный статус из items-карты).
/// Связка с ITEMS_TTL — инвариант #1346: STATUS_TTL покрывает
/// максимум возраста items-карты, до-записное значение нашей строки
/// не просачивается в гард-чтение, пока wt жив. Дрейф ниже ITEMS_TTL
/// тихо открывает стейл-окно — живая проба приёмки #1346 его вернёт.
const STATUS_TTL: std::time::Duration = std::time::Duration::from_secs(240);
const META_TTL: std::time::Duration = std::time::Duration::from_secs(600);

impl Pv2Rest {
    pub(crate) fn new(repo: String, token: String) -> Self {
        Self {
            repo,
            token,
            http: reqwest::Client::builder()
                .user_agent(concat!("uwuwu-server/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("reqwest client"),
            meta: TtlCache::new(META_TTL),
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
        Ok(RestResponse {
            status,
            link,
            body: parsed,
        })
    }

    /// Мета доски: резолв номера проекта по тайтлу + каталог Status-поля.
    pub(crate) async fn meta(&self, board: &str) -> UsecaseResult<Arc<Pv2Meta>> {
        let now = std::time::Instant::now();
        if let Some(hit) = self.meta.get(board, now) {
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
        if !projects.status.is_success() {
            return Err(UsecaseError::Board(format!(
                "projectsV2 {owner}: HTTP {}",
                projects.status
            )));
        }
        let number = projects
            .body
            .as_array()
            .and_then(|list| {
                list.iter()
                    .find(|project| project["title"] == board)
                    .and_then(|project| project["number"].as_u64())
            })
            .ok_or_else(|| {
                UsecaseError::Board(format!("project «{board}» not found in projectsV2 {owner}"))
            })?;
        let fields = self
            .rest(
                reqwest::Method::GET,
                &format!("/users/{owner}/projectsV2/{number}/fields?per_page=100"),
                None,
            )
            .await?;
        if !fields.status.is_success() {
            return Err(UsecaseError::Board(format!(
                "board «{board}»: field catalog HTTP {}",
                fields.status
            )));
        }
        let meta = parse_field_catalog(number, &fields.body, board)?;
        Ok(self.meta.put(board, meta, now))
    }

    /// Строка индекса (id + Status носителя): живой кеш → рефреш → мисса
    /// (не на доске).
    pub(crate) async fn item_entry(
        &self,
        board: &str,
        number: u64,
    ) -> UsecaseResult<Option<Pv2Item>> {
        let now = std::time::Instant::now();
        if let Some(hit) = self.items.get(board, now) {
            if let Some(item) = hit.get(&number) {
                return Ok(Some(item.clone()));
            }
        }
        let map = self.fetch_items(board).await?;
        let arc = self.items.put(board, map, now);
        Ok(arc.get(&number).cloned())
    }

    /// item id строки.
    pub(crate) async fn item_id(&self, board: &str, number: u64) -> UsecaseResult<Option<u64>> {
        Ok(self.item_entry(board, number).await?.map(|item| item.id))
    }

    async fn fetch_items(&self, board: &str) -> UsecaseResult<HashMap<u64, Pv2Item>> {
        let meta = self.meta(board).await?;
        let owner = self.owner()?;
        let mut map = HashMap::new();
        // fields=<Status field id>: list items по умолчанию несёт только
        // Title — Status-значения строк приходят только с этим параметром
        // (тест-раунд #1236: GET items/{id} слеп к Status навсегда)
        let mut url = format!(
            "/users/{owner}/projectsV2/{}/items?per_page=100&fields={}",
            meta.project_number, meta.status_field_id
        );
        for _ in 0..ITEM_PAGES {
            let resp = self.rest(reqwest::Method::GET, &url, None).await?;
            if !resp.status.is_success() {
                return Err(UsecaseError::Board(format!(
                    "board «{board}»: list items HTTP {}",
                    resp.status
                )));
            }
            if let Some(list) = resp.body.as_array() {
                for item in list {
                    if let Some((number, id, status)) = parse_item_entry(item) {
                        map.insert(number, Pv2Item { id, status });
                    }
                }
            }
            match next_after(resp.link.as_deref()) {
                Some(after) => {
                    url = format!(
                        "/users/{owner}/projectsV2/{}/items?per_page=100&fields={}&after={after}",
                        meta.project_number, meta.status_field_id
                    );
                }
                // пагинация исчерпана — индекс полный
                None => return Ok(map),
            }
        }
        // молчаливая обрезка хвоста = «строка не на доске» для живых строк
        // (ревью 2 Common-1) — громкий гард, зеркально snapshot_fetch
        Err(UsecaseError::Board(format!(
            "board «{board}»: the item index exceeded {ITEM_PAGES} pages — the guard"
        )))
    }

    /// Чтение НОСИТЕЛЯ (item index с fields-Status, мимо write-through —
    /// verify обязан подтверждать GitHub, не собственную запись):
    /// None — строки нет (мисса индекса); Some("") — Status не читается
    /// (не задан; GET items/{id} Status не отдаёт никогда — тест-раунд #1236).
    pub(crate) async fn fetch_status(
        &self,
        board: &str,
        number: u64,
    ) -> UsecaseResult<Option<String>> {
        let Some(item) = self.item_entry(board, number).await? else {
            return Ok(None);
        };
        let Some(status) = item.status else {
            // частичный entry (create-инкремент до первого полного списка):
            // id известен, статус не запрашивался — полный рефреш индекса,
            // второй заход отвечает носителем (refresh-on-status-miss,
            // живой сквозной фикса: без него гард слеп до TTL индекса)
            let map = self.fetch_items(board).await?;
            let now = std::time::Instant::now();
            let arc = self.items.put(board, map, now);
            return Ok(Some(
                arc.get(&number)
                    .and_then(|fresh| fresh.status.clone())
                    .unwrap_or_default(),
            ));
        };
        Ok(Some(status))
    }

    /// Raw-имя Status строки на доске для гарда: None — строки нет;
    /// Some("") — есть, но Status не читается (контракт прежнего
    /// carrier_status). write-through обслуживает окно read-лага
    /// после PATCH (list носителя лагает минуты, GET item слеп).
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

    /// Откат write-through статуса: verify зафиксировал Mismatch —
    /// возможно-ложное значение не доживает TTL-окно (ревью 1 Common-3:
    /// инвариант «откат write-through» обязан покрывать и этот кеш;
    /// #1437 — бюджет verify на GraphQL-риде ловит аномалии, откат
    /// остаётся семантикой громкого отказа).
    pub(crate) fn invalidate_status(&self, board: &str, number: u64) {
        self.status_wt.invalidate(&format!("{board}#{number}"));
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
        let option = resolve_status_option(&meta.options, status_name)
            .cloned()
            .ok_or_else(|| {
                UsecaseError::Board(format!("Status option not found on board: {status_name}"))
            })?;
        let owner = self.owner()?;
        let Some(item) = self.item_id(board, number).await? else {
            return Err(UsecaseError::Board(format!(
                "row #{number} is not on board «{board}»"
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
                "patch status #{number}: not applied (HTTP {})",
                resp.status
            )));
        }
        let key = format!("{board}#{number}");
        self.status_wt
            .put(&key, status_name.to_string(), std::time::Instant::now());
        Ok(parse_status_value(&resp.body["fields"]))
    }

    /// Свежий индекс привязок доски (мимо кэша, кэш обновляется): ОДИН
    /// полный фетч на вызов чистки — батч ест 1 индекс-фетч + N issue-проб,
    /// не N фетчей (квотная дисциплина общего REST-бакета ADR-113,
    /// ревью #1158 Common-1).
    pub(crate) async fn fresh_index(&self, board: &str) -> UsecaseResult<HashMap<u64, Pv2Item>> {
        let map = self.fetch_items(board).await?;
        let now = std::time::Instant::now();
        self.items.put(board, map.clone(), now);
        Ok(map)
    }

    /// DELETE item (снос строки, #1158): 204 — снос принят GitHub.
    /// Кэш-гигиена: номер уходит из живого индекса и write-through статуса
    /// (read-лаг list items не должен показывать снесённую строку).
    pub(crate) async fn delete_item(
        &self,
        board: &str,
        number: u64,
        item_id: u64,
    ) -> UsecaseResult<()> {
        let meta = self.meta(board).await?;
        let owner = self.owner()?;
        let resp = self
            .rest(
                reqwest::Method::DELETE,
                &format!(
                    "/users/{owner}/projectsV2/{}/items/{item_id}",
                    meta.project_number
                ),
                None,
            )
            .await?;
        if !resp.status.is_success() {
            return Err(UsecaseError::Board(format!(
                "delete item #{number} (item {item_id}): not applied (HTTP {})",
                resp.status
            )));
        }
        // кэш-гигиена: строка уходит из живого индекса и write-through'а;
        // пустого кэша не создаём — пустая карта «свежего индекса» морила
        // бы читателей лишними рефэтчами (ревью #1158 Low-3)
        let now = std::time::Instant::now();
        if let Some(hit) = self.items.get(board, now) {
            let mut map = (*hit).clone();
            map.remove(&number);
            self.items.put(board, map, now);
        }
        self.status_wt.invalidate(&format!("{board}#{number}"));
        Ok(())
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
                "add #{number}: not applied (HTTP {})",
                resp.status
            )));
        }
        let id = resp
            .body
            .as_object()
            .and_then(|item| item.get("id"))
            .and_then(Value::as_u64)
            .ok_or_else(|| UsecaseError::Board(format!("add #{number}: no item id")))?;
        let now = std::time::Instant::now();
        // всегда кладём id в индекс (и при пустом кеше): list items не
        // видит свежедобавленную строку в read-лаг (живая проба
        // мультиборда) — частичный map обслужит set_status сразу после
        // add; полный список дотянет refresh-on-miss чужой строки
        let mut map = match self.items.get(board, now) {
            Some(hit) => (*hit).clone(),
            None => HashMap::new(),
        };
        map.insert(number, Pv2Item { id, status: None });
        self.items.put(board, map, now);
        Ok(id)
    }
}

#[cfg(test)]
mod pv2_cache_tests;
