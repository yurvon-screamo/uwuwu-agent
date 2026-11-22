//! gh-hub adapter: доска (bulk select-колонки) + issue REST (тело/комменты/
//! нативные поля). fm-зеркало снесено (владелец 15.11): операционные данные —
//! Status/Приоритет/Automerge на доске, сессии/ветка/PR — глобальные поля issue
//! (fields.rs). Тело задачи — чистый контент.

use super::fields::{parse_field_values, Fields, IssueFields};
use super::pv2::Pv2Rest;
use super::HTTP_TIMEOUT;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use uwuwu_domain::{Status, Task};
use uwuwu_usecase::{CommentRef, Goal, GoalsRepo, IssueView, TaskBoardRepo};
use uwuwu_usecase::{UsecaseError, UsecaseResult};

pub struct GitHubHub {
    repo: String,
    token: String,
    http: reqwest::Client,
    /// Pv2 REST-мутации статусов и их пре-чтения (#1236): развод
    /// независимых GraphQL/REST бакетов — вердикт-статус не зависит
    /// от GraphQL-квоты (инстанс 16.11: task_edit падал при живом REST).
    pv2: Pv2Rest,
    /// Нативные поля issue (сессии/ветка/PR/тип).
    pub fields: Arc<IssueFields>,
    /// TTL-кеш снапшотов досок (#1243): мутации одного тика шарят снимок —
    /// без кеша каждый set_status тянул полную доску пагинацией и выжигал
    /// GraphQL-бюджет (инцидент 17.11: конвейер fail-loop до reset окна).
    snap_cache: TtlCache<(Vec<Task>, BoardMeta)>,
    /// TTL-кеш статусов строк: board_status — самый горячий GraphQL-путь
    /// (task_get/MCP), 60 сек свежести не разрывают консистентность ходов.
    status_cache: TtlCache<String>,
    /// GraphQL-бюджет: отказ RATE_LIMITED → fail-fast всех вызовов до
    /// разблокировки, без ретраев в лоб каждые poll_sec (#1243).
    gql_block: std::sync::RwLock<Option<std::time::Instant>>,
    /// Последний бюджет (remaining, resetAt) из GraphQL-ответов —
    /// наблюдаемость + контекст бэкоффа.
    last_rate_limit: std::sync::RwLock<Option<(u64, String)>>,
}

const QUERY: &str = r#"
  query($owner:String!,$name:String!,$board:String!,$after:String){
    rateLimit{ remaining resetAt }
    repository(owner:$owner, name:$name){
      projectsV2(first:20, query:$board){
        nodes{
          id title
          field(name:"Status"){ ...on ProjectV2SingleSelectField{ id options{ id name } } }
          items(first:100, after:$after){
            pageInfo{ hasNextPage endCursor }
            nodes{
              id
              content{ __typename ...on Issue{ number title state closedAt issueType{ name } } }
              fieldValues(first:30){
                nodes{ __typename
                  ...on ProjectV2ItemFieldSingleSelectValue{
                    name field{ ...on ProjectV2FieldCommon{ name } }
                  }
                  ...on ProjectV2ItemIssueFieldValue{
                    field{ ...on ProjectV2FieldCommon{ name } }
                    issueFieldValue{ __typename
                      ...on IssueFieldSingleSelectValue{ name } }
                  }
                }
              }
            }
          }
        }
      }
    }
  }"#;

const MAX_PAGES: usize = 10;

/// TTL-кеш значений по ключу (#1243): цена обращения к GitHub GraphQL
/// высока (полный снимок доски — сотни поинтов), а данные между тиками
/// меняются редко. `now` прокидывается для тестируемости.
pub(crate) struct TtlCache<T> {
    ttl: std::time::Duration,
    cells: std::sync::RwLock<std::collections::HashMap<String, (std::time::Instant, Arc<T>)>>,
}

impl<T> TtlCache<T> {
    pub(crate) fn new(ttl: std::time::Duration) -> Self {
        Self {
            ttl,
            cells: std::sync::RwLock::new(std::collections::HashMap::new()),
        }
    }

    pub(crate) fn get(&self, key: &str, now: std::time::Instant) -> Option<Arc<T>> {
        let guard = self.cells.read().expect("ttl cache lock");
        match guard.get(key) {
            Some((at, v)) if now.duration_since(*at) < self.ttl => Some(v.clone()),
            _ => None,
        }
    }

    pub(crate) fn put(&self, key: &str, value: T, now: std::time::Instant) -> Arc<T> {
        let arc = Arc::new(value);
        self.cells
            .write()
            .expect("ttl cache lock")
            .insert(key.to_string(), (now, arc.clone()));
        arc
    }

    /// Точечный сброс ключа: мутация доски — read-your-writes (#1235).
    pub(crate) fn invalidate(&self, key: &str) {
        self.cells.write().expect("ttl cache lock").remove(key);
    }

    /// Сброс всех ключей: тайтл строки живёт в снапшотах всех досок.
    fn invalidate_all(&self) {
        self.cells.write().expect("ttl cache lock").clear();
    }
}

/// Исход verify-after-write (#1235, семья #1213).
enum VerifyOutcome {
    /// Чтение подтвердило запись.
    Ok,
    /// Rate limit: ретраи не продолжаем — мутация принята GitHub,
    /// write-through сохранён (окно возможно-ложного кэша ограничено TTL).
    SkippedRateLimited,
    /// Три чтения не подтвердили запись — громкий отказ + откат write-through.
    Mismatch,
}

/// Единый тег классификации в тексте отказа gql-транспорта: kind —
/// «RATE_LIMITED» (Core/Secondary и GraphQL-лимит) или «PERMISSION»
/// (настоящий отказ доступа). Verify-логика матчит ТОЛЬКО этот тег,
/// не тексты GitHub — иначе Permission-отказ («не rate limit» в Display)
/// читался как rate limit (код-ревью #1235 Common-1).
fn gh_tagged(fail: &crate::gherr::GhFail) -> UsecaseError {
    let kind = match fail {
        crate::gherr::GhFail::Permission => "PERMISSION",
        crate::gherr::GhFail::Core { .. } | crate::gherr::GhFail::Secondary { .. } => {
            "RATE_LIMITED"
        }
    };
    UsecaseError::Board(format!("gh graphql [{kind}]: {fail}"))
}

/// Rate-limit отказ транспорта: только собственные теги gql/pv2-точек,
/// не строковые эвристики по текстам GitHub.
fn is_rate_limited(e: &UsecaseError) -> bool {
    let text = e.to_string().to_lowercase();
    text.contains("gh graphql [rate_limited]") || text.contains("gh rest [rate_limited]")
}

/// Транспортный флап gql-точки (обрыв соединения, усечение тела, не-JSON,
/// 5xx): единый тег [TRANSPORT] в тексте отказа (#1330) — по паттерну
/// gh_tagged/is_rate_limited, не строковые эвристики по текстам GitHub.
fn gh_transport_tagged(detail: String) -> UsecaseError {
    UsecaseError::Board(format!("gh graphql [TRANSPORT]: {detail}"))
}

/// Транспортный класс отказа gql-чтения: тег [TRANSPORT] — контракт;
/// rate limit/permission и сырые не-тегированные тексты классом не считаются.
fn is_transport_flap(e: &UsecaseError) -> bool {
    e.to_string()
        .to_lowercase()
        .contains("gh graphql [transport]")
}

/// Политика короткого ретрая чтения (#1330): разовый транспортный флап —
/// одна повторная попытка после паузы; rate limit/permission (ADR-110),
/// graphql errors и исчерпание попыток — Err наверх как есть. Константы
/// кода сознательно (прецедент #1281): всего 2 попытки, пауза 3 сек.
const TRANSPORT_RETRY_PAUSE: std::time::Duration = std::time::Duration::from_secs(3);
const TRANSPORT_RETRY_ATTEMPTS: u32 = 2;

fn retry_after_flap(e: &UsecaseError, attempt: u32) -> Option<std::time::Duration> {
    if attempt >= TRANSPORT_RETRY_ATTEMPTS || !is_transport_flap(e) {
        return None;
    }
    Some(TRANSPORT_RETRY_PAUSE)
}

#[derive(Default, Clone)]
struct BoardMeta {
    project_id: String,
    status_field_id: String,
    options: std::collections::HashMap<String, String>,
    /// issue number → GraphQL node id строки (PVTI_): путь данных
    /// archive_row (#1293) — мутационная мета снапшота.
    items: std::collections::HashMap<u64, String>,
}

impl GitHubHub {
    pub fn new(repo: String, token: String) -> Self {
        let pv2 = Pv2Rest::new(repo.clone(), token.clone());
        Self {
            fields: Arc::new(IssueFields::new(repo.clone(), token.clone())),
            pv2,
            repo,
            token,
            http: reqwest::Client::builder()
                .user_agent(concat!("uwuwu-server/", env!("CARGO_PKG_VERSION")))
                .timeout(HTTP_TIMEOUT)
                .build()
                .expect("reqwest client"),
            // 240 c > тик (poll 180 c): снимок живёт через тик — полный
            // рефреш раз в два тика (−50% GraphQL-трафика; доска 746 строк
            // выжигала часовой бюджет, инцидент 18.11), мутации шарят
            // снимок. Свежесть статусов для decide — ≤ двух тиков.
            snap_cache: TtlCache::new(std::time::Duration::from_secs(240)),
            status_cache: TtlCache::new(std::time::Duration::from_secs(60)),
            gql_block: std::sync::RwLock::new(None),
            last_rate_limit: std::sync::RwLock::new(None),
        }
    }

    pub fn fields(&self) -> Arc<IssueFields> {
        self.fields.clone()
    }

    /// Бюджет заблокирован до момента (#1243): RATE_LIMITED → пауза,
    /// чтобы тик-луп не молотил GraphQL вхолостую каждые poll_sec.
    fn gql_blocked(&self, now: std::time::Instant) -> Option<std::time::Instant> {
        self.gql_block
            .read()
            .expect("gql block lock")
            .filter(|until| *until > now)
    }

    fn block_gql_until(&self, until: std::time::Instant) {
        *self.gql_block.write().expect("gql block lock") = Some(until);
    }

    async fn gql(&self, query: &str, variables: Value) -> UsecaseResult<Value> {
        let now = std::time::Instant::now();
        if let Some(until) = self.gql_blocked(now) {
            return Err(UsecaseError::Board(format!(
                "gh graphql [RATE_LIMITED]: backoff for another {:?}",
                until.duration_since(now)
            )));
        }
        let resp = self
            .http
            .post("https://api.github.com/graphql")
            .bearer_auth(&self.token)
            .json(&json!({ "query": query, "variables": variables }))
            .send()
            .await
            .map_err(|e| gh_transport_tagged(format!("connection failure: {e}")))?;
        let status = resp.status();
        let headers = resp.headers().clone();
        let text = resp
            .text()
            .await
            .map_err(|e| gh_transport_tagged(format!("response read cut off: {e}")))?;
        if let Some(fail) = crate::gherr::classify(status, &headers) {
            return Err(gh_tagged(&fail));
        }
        let body: Value = serde_json::from_str(&text)
            .map_err(|e| gh_transport_tagged(format!("not JSON (HTTP {status}): {e}")))?;
        if !status.is_success() {
            // 5xx — транспортный класс (#1330: 502/503/504 с валидным телом
            // ретраятся наравне с обрывами); прочие не-успехи — как раньше
            return Err(if status.is_server_error() {
                gh_transport_tagged(format!("HTTP {status}: {}", trunc(&text)))
            } else {
                UsecaseError::Board(format!("gh api {status}: {}", trunc(&text)))
            });
        }
        if let Some(errors) = body.get("errors") {
            // rate limit: детект — одна точка gherr (оба формата GitHub:
            // легаси RATE_LIMITED и новый RATE_LIMIT — #1243/#1156).
            // Классифицированный отказ громкий; бэкофф-блок 5 мин поверх —
            // окно rolling, лобовые ретраи каждые poll_sec только шумят
            if let Some(fail) = crate::gherr::classify_gql_errors(&body) {
                let until = now + std::time::Duration::from_secs(5 * 60);
                self.block_gql_until(until);
                tracing::warn!(
                    event = "gql.rate_limited",
                    until = ?until,
                    "GraphQL rate limit: бэкофф 5 мин (снапшоты без кеша выжигали бюджет — #1243)"
                );
                return Err(gh_tagged(&fail));
            }
            return Err(UsecaseError::Board(format!("graphql errors: {errors}")));
        }
        body.get("data")
            .cloned()
            .ok_or_else(|| UsecaseError::Board("graphql: no data".into()))
    }

    /// Чтение gql с коротким ретраем (#1330): разовый транспортный флап
    /// (502/обрыв/усечение) не роняет тик. Только идемпотентные чтения —
    /// мутации зовут gql() напрямую: 502 после успешного применения =
    /// риск двойного применения. Rate limit/permission не ретраятся
    /// (ADR-110: немедленный громкий Err), gql_block не задет.
    async fn gql_retrying(&self, query: &str, variables: Value) -> UsecaseResult<Value> {
        match self.gql(query, variables.clone()).await {
            Ok(data) => Ok(data),
            Err(first) => match retry_after_flap(&first, 1) {
                None => Err(first),
                Some(pause) => {
                    tracing::warn!(
                        event = "gql.transport_retry",
                        pause_secs = pause.as_secs(),
                        error = %first,
                        "транспортный флап gql-чтения — короткий ретрай (#1330)"
                    );
                    tokio::time::sleep(pause).await;
                    // вторая попытка — последняя: её Err наверх как есть
                    self.gql(query, variables).await
                }
            },
        }
    }

    async fn rest(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> UsecaseResult<(reqwest::StatusCode, Value)> {
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
            .map_err(|e| UsecaseError::Board(format!("rest transport {path}: {e}")))?;
        let status = resp.status();
        let headers = resp.headers().clone();
        let text = resp
            .text()
            .await
            .map_err(|e| UsecaseError::Board(format!("rest body {path}: {e}")))?;
        if let Some(fail) = crate::gherr::classify(status, &headers) {
            return Err(UsecaseError::Board(format!("gh api {path}: {fail}")));
        }
        let value = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::Null)
        };
        Ok((status, value))
    }

    /// Снимок доски: bulk-задачи (Status/Приоритет/Automerge) + мета мутаций.
    /// TTL-кеш (#1243): тик и его мутации шарят один снимок. Строка,
    /// добавленная на доску младше TTL, может не иметь item-id в мете —
    /// мутация по ней ошибётся и самолечится следующим тиком (свежий снимок).
    async fn snapshot(&self, board: &str) -> UsecaseResult<Arc<(Vec<Task>, BoardMeta)>> {
        let now = std::time::Instant::now();
        if let Some(hit) = self.snap_cache.get(board, now) {
            return Ok(hit);
        }
        let fetched = self.snapshot_fetch(board).await?;
        // бюджетная наблюдаемость: remaining несёт каждый снимок (QUERY)
        if let Some(rl) = self.last_rate_limit.read().expect("rl lock").as_ref() {
            let remaining = rl.0;
            if remaining < 300 {
                tracing::warn!(
                    event = "gql.budget_low",
                    remaining,
                    reset_at = %rl.1,
                    "GraphQL-бюджет на исходе"
                );
            }
        }
        let arc = self.snap_cache.put(board, fetched, now);
        Ok(arc)
    }

    /// Полное чтение доски пагинацией (мимо кеша).
    async fn snapshot_fetch(&self, board: &str) -> UsecaseResult<(Vec<Task>, BoardMeta)> {
        let (owner, name) = self.repo.split_once('/').ok_or_else(|| {
            UsecaseError::Board(format!("repo in the owner/name form: {}", self.repo))
        })?;
        let mut meta = BoardMeta::default();
        let mut rows: Vec<Task> = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let data = self
                .gql_retrying(
                    QUERY,
                    json!({ "owner": owner, "name": name, "board": board, "after": cursor }),
                )
                .await?;
            // бюджет с каждого снимка (QUERY несёт rateLimit) — #1243
            if let Some(rl) = data.get("rateLimit") {
                if let Some(remaining) = rl["remaining"].as_u64() {
                    let reset_at = rl["resetAt"].as_str().unwrap_or_default().to_string();
                    *self.last_rate_limit.write().expect("rl lock") = Some((remaining, reset_at));
                }
            }
            let nodes = data["repository"]["projectsV2"]["nodes"]
                .as_array()
                .ok_or_else(|| UsecaseError::Board("projectsV2.nodes is not an array".into()))?;
            let project = nodes
                .iter()
                .find(|p| p["title"].as_str() == Some(board))
                .ok_or_else(|| {
                    UsecaseError::Board(format!(
                        "board «{board}» not found among {} boards",
                        nodes.len()
                    ))
                })?;
            if meta.project_id.is_empty() {
                meta.project_id = project["id"].as_str().unwrap_or_default().to_string();
                meta.status_field_id = project["field"]["id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                if let Some(options) = project["field"]["options"].as_array() {
                    for o in options {
                        if let (Some(id), Some(n)) = (o["id"].as_str(), o["name"].as_str()) {
                            meta.options.insert(n.to_string(), id.to_string());
                        }
                    }
                }
                if meta.project_id.is_empty() {
                    return Err(UsecaseError::Board(format!(
                        "board «{board}»: no project id"
                    )));
                }
            }
            let items = &project["items"];
            if let Some(page) = items["nodes"].as_array() {
                rows.extend(parse_page(page, &mut meta));
            }
            if items["pageInfo"]["hasNextPage"].as_bool() != Some(true) {
                return Ok((rows, meta));
            }
            cursor = items["pageInfo"]["endCursor"].as_str().map(String::from);
        }
        Err(UsecaseError::Board(format!(
            "board «{board}» exceeded the {MAX_PAGES} pages — the guard"
        )))
    }

    /// Статус строки на доске (по номеру issue, через projectItems).
    async fn board_status(&self, number: u64) -> UsecaseResult<String> {
        // TTL-кеш (#1243): task_get/MCP — самый горячий GraphQL-путь;
        // 60 сек свежести не разрывают консистентность ходов конвейера
        let key = number.to_string();
        let now = std::time::Instant::now();
        if let Some(hit) = self.status_cache.get(&key, now) {
            return Ok((*hit).clone());
        }
        let status = self.board_status_fetch(number).await?;
        self.status_cache.put(&key, status.clone(), now);
        Ok(status)
    }

    async fn board_status_fetch(&self, number: u64) -> UsecaseResult<String> {
        let q = r#"
          query($owner:String!,$name:String!,$n:Int!){
            repository(owner:$owner,name:$name){
              issue(number:$n){
                projectItems(first:3){
                  nodes{ fieldValues(first:20){ nodes{
                    __typename ...on ProjectV2ItemFieldSingleSelectValue{
                      name field{ ...on ProjectV2FieldCommon{ name } } } } } }
                }
              }
            }
          }"#;
        let (owner, name) = self
            .repo
            .split_once('/')
            .ok_or_else(|| UsecaseError::Board(format!("repo: {}", self.repo)))?;
        let data = self
            .gql_retrying(
                q,
                json!({ "owner": owner, "name": name, "n": number as i64 }),
            )
            .await?;
        if let Some(items) = data["repository"]["issue"]["projectItems"]["nodes"].as_array() {
            for item in items {
                if let Some(vals) = item["fieldValues"]["nodes"].as_array() {
                    for v in vals {
                        if v["field"]["name"].as_str() == Some("Status") {
                            return Ok(v["name"].as_str().unwrap_or_default().to_string());
                        }
                    }
                }
            }
        }
        Ok(String::new())
    }

    /// state issue (closed?) — REST issues GET (core-бакет, не GraphQL).
    /// Гард-чтение row_status резолвит им терминальность расщеплённых
    /// строк (#1403): closed → Done независимо от отставшего поля.
    async fn issue_state_closed(&self, number: u64) -> UsecaseResult<bool> {
        let (_, issue) = self
            .rest(
                reqwest::Method::GET,
                &format!("/repos/{}/issues/{number}", self.repo),
                None,
            )
            .await?;
        Ok(issue["state"].as_str() == Some("closed"))
    }

    /// Статусный value строки КОНКРЕТНОЙ доски читает Pv2Rest
    /// (pv2::read_status — гард-семантика с write-through); прежний
    /// GraphQL carrier_status снён с переводом гард-пути на REST (#1236).
    /// Сброс снимка доски: мутация могла изменить строки/мету (#1235).
    fn invalidate_snapshot(&self, board: &str) {
        self.snap_cache.invalidate(board);
    }

    /// Write-through известного статуса в снапшот (#1235): клон с
    /// обновлённой строкой под тем же ключом — read-лаг Projects v2
    /// минутного масштаба не отдаёт собственную запись старой.
    fn write_through_status(&self, board: &str, number: u64, status: Status) {
        let now = std::time::Instant::now();
        let Some(hit) = self.snap_cache.get(board, now) else {
            return;
        };
        let (mut rows, meta) = hit.as_ref().clone();
        for task in rows.iter_mut().filter(|t| t.number() == number) {
            task.override_status(status);
        }
        self.snap_cache.put(board, (rows, meta), now);
    }

    /// Write-through статусного кэша строки (task_get читает отсюда).
    fn write_through_status_cache(&self, number: u64, status: Status) {
        let now = std::time::Instant::now();
        self.status_cache.put(
            &number.to_string(),
            uwuwu_usecase::status_name(status).to_string(),
            now,
        );
    }

    /// Кеш-гигиена снапшота после записи нестатусных ячеек (#1258):
    /// строка в кешированном снимке — write-through записанных значений
    /// (read-your-writes, без рефетча и расхода GraphQL-бюджета);
    /// снимка нет или строки в нём нет — инвалидация (окно create:
    /// add_to_board инвалидирует до записи полей — свежий фетч мог
    /// закешироваться без полей).
    fn settle_board_cells(
        &self,
        board: &str,
        number: u64,
        priority: Option<&str>,
        automerge: Option<bool>,
        direct: Option<bool>,
    ) {
        let now = std::time::Instant::now();
        let Some(hit) = self.snap_cache.get(board, now) else {
            return; // снимка нет — нечего гигиенить, фетч увидит поля
        };
        let (mut rows, meta) = hit.as_ref().clone();
        let row_present = rows.iter_mut().any(|t| t.number() == number);
        if !row_present {
            // окно create: снимок мог закешироваться без полей — сброс,
            // следующий фетч несёт записанные значения
            self.invalidate_snapshot(board);
            return;
        }
        for task in rows.iter_mut().filter(|t| t.number() == number) {
            if let Some(p) = priority {
                task.set_priority(uwuwu_usecase::parse_priority(p));
            }
            if let Some(a) = automerge {
                task.set_automerge(a);
            }
            if let Some(d) = direct {
                task.set_direct(d);
            }
        }
        self.snap_cache.put(board, (rows, meta), now);
    }

    /// Контрольное чтение после мутации статуса (семья #1213): ответ
    /// мутации с искомым значением — быстрый путь без доп. запросов;
    /// иначе до 3 чтений НОСИТЕЛЯ (мимо write-through — иначе verify
    /// самоподтверждался бы собственной записью) с паузой 1 с.
    /// Rate limit — ретраи не продолжаются (мутация принята GitHub);
    /// сетевой сбой и рассогласование — полный цикл, дальше громкий отказ.
    async fn verify_status(
        &self,
        board: &str,
        number: u64,
        expected: Status,
        confirmed: Option<String>,
    ) -> VerifyOutcome {
        // сравнение доменное, не строковое: в окне merge→rename (#1383)
        // носитель отвечает легаси-русским именем опции при английском каноне
        if confirmed
            .as_deref()
            .is_some_and(|raw| uwuwu_usecase::parse_status(raw) == expected)
        {
            return VerifyOutcome::Ok;
        }
        for attempt in 0..3 {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            match self.pv2.fetch_status(board, number).await {
                Ok(Some(raw)) if uwuwu_usecase::parse_status(&raw) == expected => {
                    return VerifyOutcome::Ok;
                }
                Err(e) if is_rate_limited(&e) => return VerifyOutcome::SkippedRateLimited,
                _ => {}
            }
        }
        VerifyOutcome::Mismatch
    }

    /// Пост-мутационная гигиена кэша по исходу verify (код-ревью #1235
    /// Common-3): write-through уже стоит — исход решает, остаётся ли он.
    /// Ok/SkippedRateLimited → кэш держит записанное значение (окно
    /// возможно-ложного значения при rate-limit ограничено TTL);
    /// Mismatch → откат write-through (инвалидация) + громкий отказ —
    /// не держим возможно-ложное значение дольше TTL wt (240 с, #1346;
    /// семья #1213).
    fn settle_write_through(
        &self,
        board: &str,
        number: u64,
        outcome: VerifyOutcome,
        status: Status,
    ) -> UsecaseResult<()> {
        match outcome {
            VerifyOutcome::Ok => {}
            VerifyOutcome::SkippedRateLimited => tracing::warn!(
                event = "board.verify_skipped",
                number,
                to = %uwuwu_usecase::status_name(status),
                "verify пропущен (rate limit): мутация принята GitHub, write-through сохранён"
            ),
            VerifyOutcome::Mismatch => {
                self.invalidate_snapshot(board);
                self.status_cache.invalidate(&number.to_string());
                // write-through Pv2Rest — тот же инвариант: возможно-ложное
                // значение не доживает TTL-окно (ревью 1 Common-3)
                self.pv2.invalidate_status(board, number);
                return Err(UsecaseError::Board(format!(
                    "status #{number}: write «{}» not confirmed by read — \
                     GitHub lag or mutation not applied; do not retry blindly",
                    uwuwu_usecase::status_name(status)
                )));
            }
        }
        Ok(())
    }

    /// Комменты задачи: свежий хвост (direction=desc, последние 100),
    /// возврат по возрастанию. GitHub без direction отдаёт старейшие —
    /// wakeup был слеп к свежим комментам при длинных тредах (раунд 3).
    async fn all_comments(&self, number: u64) -> UsecaseResult<Vec<CommentRef>> {
        let (status, body) = self
            .rest(
                reqwest::Method::GET,
                &format!(
                    "/repos/{}/issues/{number}/comments?per_page=100&direction=desc",
                    self.repo
                ),
                None,
            )
            .await?;
        if !status.is_success() {
            return Err(UsecaseError::Board(format!(
                "comments #{number}: HTTP {status}"
            )));
        }
        let mut out: Vec<CommentRef> = body
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(|c| {
                        let created =
                            chrono::DateTime::parse_from_rfc3339(c["created_at"].as_str()?).ok()?;
                        Some(CommentRef {
                            id: c["node_id"].as_str()?.to_string(),
                            created_ms: created.timestamp_millis() as u64,
                            body: c["body"].as_str().unwrap_or_default().to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.sort_by_key(|c| c.created_ms);
        Ok(out)
    }
}

/// Гард-резолюция статуса строки (#1403): закрытый issue терминален
/// независимо от отставшего Pv2-поля (UC-07, то же правило «closed
/// терминален», что в read_issue/parse_page). state не читался (None —
/// отказ носителя) — деградация к парсу поля, прежнее поведение.
fn guard_status(parsed: Status, issue_closed: Option<bool>) -> Status {
    match issue_closed {
        Some(true) => Status::Done,
        _ => parsed,
    }
}

fn parse_page(items: &[Value], meta: &mut BoardMeta) -> Vec<Task> {
    items
        .iter()
        .filter_map(|item| {
            let issue = &item["content"];
            if issue["__typename"].as_str()? != "Issue" {
                return None;
            }
            let number = issue["number"].as_u64()?;
            let title = issue["title"].as_str()?.to_string();
            let v = item_values(item);
            // closed issue терминален независимо от отстающего поля доски
            // (вывод первоисточника, не переход конвейера — потому конструктор)
            let status = if issue["state"].as_str() == Some("closed") {
                Status::Done
            } else {
                uwuwu_usecase::parse_status(&v.status)
            };
            let mut task = Task::new(number, title, status);
            // Дата закрытия issue — вход клина холодного ящика (#1293)
            if let Some(raw) = issue["closedAt"].as_str() {
                if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(raw) {
                    task.set_closed_at(ts.timestamp_millis() as u64);
                }
            }
            task.set_priority(uwuwu_usecase::parse_priority(&v.priority));
            task.set_automerge(v.automerge);
            task.set_direct(v.direct);
            // тип — нативный issue Type на issue-узле (не ячейка доски;
            // #1271): null/Task/неизвестное имя → None
            task.set_kind(uwuwu_usecase::parse_task_type(
                issue["issueType"]["name"].as_str().unwrap_or_default(),
            ));
            meta.items
                .insert(number, item["id"].as_str().unwrap_or_default().to_string());
            Some(task)
        })
        .collect()
}

/// Bulk-значения select-колонок доски (поля issue ленивые — fields.rs).
struct ItemValues {
    status: String,
    priority: String,
    automerge: bool,
    direct: bool,
}

fn item_values(item: &Value) -> ItemValues {
    let mut out = ItemValues {
        status: String::new(),
        priority: String::new(),
        automerge: false,
        direct: false,
    };
    if let Some(vals) = item["fieldValues"]["nodes"].as_array() {
        for v in vals {
            let field = v["field"]["name"].as_str().unwrap_or_default();
            // Pv2-own поля несут значение на верхнем уровне узла;
            // issue-backed (каталог /user/issue-fields: «Приоритет»,
            // «Automerge») — вложенным issueFieldValue (#1174)
            let value = v["name"]
                .as_str()
                .or(v["issueFieldValue"]["name"].as_str())
                .unwrap_or_default();
            match field {
                "Status" => out.status = value.to_string(),
                crate::github::fields::F_PRIORITY | "Приоритет" => {
                    out.priority = value.to_string()
                }
                // автомерж (#1232): новое имя, legacy «Autodone» — back-compat
                "Automerge" | "Autodone" => out.automerge = value == "on" || value == "true",
                uwuwu_usecase::F_DIRECT => out.direct = value == "on",
                _ => {}
            }
        }
    }
    out
}

fn trunc(s: &str) -> String {
    s.chars().take(200).collect()
}

/// Маппинг сырых имён полей → типизированный TaskFields (домен не знает имён;
/// невалидные значения полей (не-ses_ id, не-http URL) → None: мусор в ячейке
/// не становится заявкой — валидация в value-объектах домена).
fn map_task_fields(raw: &Fields) -> uwuwu_domain::TaskFields {
    use uwuwu_domain::{Slot, TaskFields, Vcs};
    let s = |v: Option<&String>| v.and_then(|s| uwuwu_domain::SessionRef::parse(s));
    // слоты: dev/review/test; review-поле покрывает оба ревью-слота,
    // dev-поле — оба слота дев-семьи (Design/Dev — единая сессия, канон
    // дев-семьи — usecase/pipeline/tick.rs «едина между этапами и
    // откатами (требование владельца)», носитель — семейные поля
    // fields.rs + set_session): иначе Design-строка читается без
    // сессии и тик бесконечно пытается спавнить дубль (инцидент
    // 15.11/16.11 — budget-отказы маскировали, budget-50 вскрыл бы
    // спавном дублей)
    let mut assignments = std::collections::BTreeMap::new();
    for (name, slots) in [
        (
            crate::github::fields::F_DEV_SESSION,
            vec![Slot::Dev, Slot::Design],
        ),
        (
            crate::github::fields::F_REVIEW_SESSION,
            vec![Slot::PlanReview, Slot::ResultReview, Slot::TestReview],
        ),
        (crate::github::fields::F_TEST_SESSION, vec![Slot::Test]),
    ] {
        if let Some(session) = s(raw.get(name)) {
            for slot in slots {
                assignments.insert(slot, session.clone());
            }
        }
    }
    // Приоритет с носителя: один парсер со snapshot-путём (naming::
    // parse_priority — trim/регистр/русские варианты); неизвестное — Empty.
    // Ключ канона + легаси «Приоритет» (контракт окна merge→rename #1383:
    // физическое issue-поле может нести старый лейбл до rename владельцем).
    // Без мапинга гейт пика морозил бы ВСЕ спавны, включая high
    // (инцидент #1170: high-таска deferred_peak'илась, ревьювер не
    // поднимался в окно GLM).
    let priority = raw
        .get(crate::github::fields::F_PRIORITY)
        .or_else(|| raw.get("Приоритет"))
        .map(|v| uwuwu_usecase::parse_priority(v))
        .unwrap_or(uwuwu_domain::Priority::Empty);
    // VCS-жизненный цикл явный; пара (Branch, PR) — кодовый цикл с веткой.
    // Безветочная PR-ячейка — Artifact (канон ADR-021: ячейка несёт URL
    // и без ветки кода — вики/issue/чужой-вендор PR; #1155 — рендер,
    // не отброс). Кодовый инвариант «PR требует ветку» живёт в мутаторах
    // домена (open_pr), не в загрузчике носителя.
    let vcs = match (
        raw.get(crate::github::fields::F_BRANCH)
            .and_then(|s| uwuwu_domain::BranchName::parse(s)),
        raw.get("PR").and_then(|s| uwuwu_domain::PrUrl::parse(s)),
    ) {
        (Some(branch), Some(url)) => Vcs::PullRequest { branch, url },
        (Some(branch), None) => Vcs::Branch(branch),
        (None, Some(url)) => Vcs::Artifact { url },
        (None, None) => Vcs::Local,
    };
    TaskFields::new(assignments, vcs, priority)
}

#[async_trait]
impl TaskBoardRepo for GitHubHub {
    async fn read_board(&self, board: &str) -> UsecaseResult<Vec<Task>> {
        Ok(self.snapshot(board).await?.0.clone())
    }

    async fn set_status(&self, board: &str, number: u64, status: Status) -> UsecaseResult<()> {
        let name = uwuwu_usecase::status_name(status);
        // last-known статус строки — для ребра в логах (Low-2 код-ревью
        // #1235); источник — локальный статусный кеш, GraphQL не тратим
        let from = self
            .status_cache
            .get(&number.to_string(), std::time::Instant::now())
            .map(|raw| raw.as_str().to_string());
        // мутация — REST ProjectsV2 (#1236); write-through Pv2Rest
        // обслуживает повторный гард в окне read-лага носителя
        let confirmed = self.pv2.set_status(board, number, name).await?;
        // read-your-writes (#1235): собственная запись обслуживается кэшами,
        // read-лаг Projects v2 минутного масштаба не виден.
        self.write_through_status(board, number, status);
        self.write_through_status_cache(number, status);
        // verify-after-write (семья #1213): ответ мутации — быстрый путь,
        // иначе до 3 чтений носителя; rate limit — ретраи не продолжаются.
        let outcome = self.verify_status(board, number, status, confirmed).await;
        self.settle_write_through(board, number, outcome, status)?;
        tracing::info!(
            event = "board.set_status",
            board,
            number,
            from = from.as_deref().unwrap_or("—"),
            to = %name,
            "Статус строки обновлён и подтверждён чтением (from — last-known из кеша)"
        );
        Ok(())
    }

    async fn row_status(&self, board: &str, number: u64) -> UsecaseResult<Option<Status>> {
        // Pv2 REST (#1236): гард канона edit() больше не зависит от
        // GraphQL-бакета; project-фильтр — индекс этой доски (прочая
        // привязка строки — не наша строка, Low-1 код-ревью #1235)
        match self.pv2.read_status(board, number).await? {
            // Status-значения на носителе нет (строка не на доске /
            // поле не задано / read-лаг); непустое непарсящееся — Some(Empty)
            None => Ok(None),
            Some(raw) if raw.trim().is_empty() => Ok(None),
            Some(raw) => {
                let parsed = uwuwu_usecase::parse_status(&raw);
                if parsed == Status::Done {
                    // поле уже терминально — state не читаем (бюджет REST)
                    return Ok(Some(Status::Done));
                }
                // Терминальная резолюция (#1403): issue, закрытый мимо
                // task_close, терминален независимо от отставшего поля —
                // гард edit() режет фантомные переходы громкой канон-ошибкой
                // вместо тихой записи в закрытый issue
                let closed = match self.issue_state_closed(number).await {
                    Ok(closed) => Some(closed),
                    Err(e) => {
                        tracing::warn!(
                            event = "row_status.state_unreadable",
                            number,
                            error = %e,
                            "state issue не читался — гард отвечает парсом поля (#1403)"
                        );
                        None
                    }
                };
                Ok(Some(guard_status(parsed, closed)))
            }
        }
    }

    async fn row_exists(&self, board: &str, number: u64) -> UsecaseResult<bool> {
        Ok(self.pv2.item_id(board, number).await?.is_some())
    }

    async fn set_session(
        &self,
        _board: &str,
        number: u64,
        slot: uwuwu_domain::Slot,
        session: &str,
    ) -> UsecaseResult<()> {
        let name = match slot {
            uwuwu_domain::Slot::Dev => crate::github::fields::F_DEV_SESSION,
            uwuwu_domain::Slot::PlanReview
            | uwuwu_domain::Slot::ResultReview
            | uwuwu_domain::Slot::TestReview => crate::github::fields::F_REVIEW_SESSION,
            uwuwu_domain::Slot::Test => crate::github::fields::F_TEST_SESSION,
            uwuwu_domain::Slot::Design => crate::github::fields::F_DEV_SESSION,
        };
        self.fields
            .store(number, &[(name, Some(session.to_string()))])
            .await?;
        self.invalidate_snapshot(_board);
        Ok(())
    }

    async fn set_branch(
        &self,
        _board: &str,
        number: u64,
        branch: &uwuwu_domain::BranchName,
    ) -> UsecaseResult<()> {
        self.fields
            .store(
                number,
                &[(
                    crate::github::fields::F_BRANCH,
                    Some(branch.as_str().to_string()),
                )],
            )
            .await?;
        self.invalidate_snapshot(_board);
        Ok(())
    }

    async fn set_board_cells(
        &self,
        board: &str,
        number: u64,
        priority: Option<&str>,
        automerge: Option<bool>,
        direct: Option<bool>,
    ) -> UsecaseResult<()> {
        // поля — нативные issue (REST), как и у set_branch
        let mut patch: Vec<(&str, Option<String>)> = Vec::new();
        if let Some(p) = priority {
            patch.push((
                crate::github::fields::F_PRIORITY,
                Some(p.trim().to_string()),
            ));
        }
        if let Some(a) = automerge {
            patch.push((
                uwuwu_usecase::F_AUTOMERGE,
                Some(if a { "on" } else { "off" }.to_string()),
            ));
        }
        if let Some(d) = direct {
            patch.push((
                uwuwu_usecase::F_DIRECT,
                Some(if d { "on" } else { "off" }.to_string()),
            ));
        }
        if !patch.is_empty() {
            self.fields.store(number, &patch).await?;
        }
        // read-your-writes (#1258): кеш-гигиена снапшота — write-through
        // записанных значений при живой строке, инвалидация при отсутствующей
        self.settle_board_cells(board, number, priority, automerge, direct);
        Ok(())
    }

    async fn clear_session(
        &self,
        _board: &str,
        number: u64,
        slot: uwuwu_domain::Slot,
    ) -> UsecaseResult<()> {
        let name = match slot {
            uwuwu_domain::Slot::Dev | uwuwu_domain::Slot::Design => {
                crate::github::fields::F_DEV_SESSION
            }
            uwuwu_domain::Slot::PlanReview
            | uwuwu_domain::Slot::ResultReview
            | uwuwu_domain::Slot::TestReview => crate::github::fields::F_REVIEW_SESSION,
            uwuwu_domain::Slot::Test => crate::github::fields::F_TEST_SESSION,
        };
        self.fields.store(number, &[(name, None)]).await?;
        self.invalidate_snapshot(_board);
        Ok(())
    }

    async fn post_comment(&self, number: u64, body: &str) -> UsecaseResult<String> {
        let (status, resp) = self
            .rest(
                reqwest::Method::POST,
                &format!("/repos/{}/issues/{number}/comments", self.repo),
                Some(json!({ "body": body })),
            )
            .await?;
        if !status.is_success() {
            return Err(UsecaseError::Board(format!(
                "comment #{number}: HTTP {status}"
            )));
        }
        // node_id — та же идентичность, что в журнале latest_comments;
        // пусто (нестандартный ответ) — маркировка невозможна, fail-open.
        Ok(resp["node_id"].as_str().unwrap_or_default().to_string())
    }

    async fn close_issue(&self, number: u64) -> UsecaseResult<()> {
        self.close_issue_ext(number, false).await
    }

    async fn read_issue(&self, _board: &str, number: u64) -> UsecaseResult<IssueView> {
        let (_, issue) = self
            .rest(
                reqwest::Method::GET,
                &format!("/repos/{}/issues/{number}", self.repo),
                None,
            )
            .await?;
        let title = issue["title"].as_str().unwrap_or_default().to_string();
        let body = issue["body"].as_str().unwrap_or_default().to_string();
        let status = self.board_status(number).await.unwrap_or_default();
        // closed терминален (вывод первоисточника, не переход конвейера)
        let status = if issue["state"].as_str() == Some("closed") {
            Status::Done
        } else {
            uwuwu_usecase::parse_status(&status)
        };
        let raw = parse_field_values(&issue);
        let mut task = Task::new(number, title, status);
        task.apply_fields(map_task_fields(&raw));
        // automerge с носителя (fm-вью честен, баг-репорт тестировщика #1232:
        // read_issue не мапил значение — task_get врал automerge: false);
        // тик-путь (snapshot item_values) уже читал, это REST-путь MCP
        let automerge = raw
            .get("Automerge")
            .or_else(|| raw.get("Autodone"))
            .map(|v| v == "on" || v == "true")
            .unwrap_or(false);
        task.set_automerge(automerge);
        // direct с носителя (#1357) — симметрично automerge: REST-путь MCP
        // обязан быть честен, тик-путь читает item_values
        let direct = raw
            .get(uwuwu_usecase::F_DIRECT)
            .map(|v| v == "on")
            .unwrap_or(false);
        task.set_direct(direct);
        // тип — нативный issue Type (верхний уровень GET issue, не
        // issue_field_values; #1271) — REST-путь MCP честен, как тик-путь
        task.set_kind(uwuwu_usecase::parse_task_type(
            issue["type"]["name"].as_str().unwrap_or_default(),
        ));
        let comments = self.all_comments(number).await?;
        Ok(IssueView {
            task,
            body,
            comments,
        })
    }

    async fn create_issue(
        &self,
        board: &str,
        project: &str,
        title: &str,
        body: &str,
        kind_name: Option<&str>,
    ) -> UsecaseResult<u64> {
        let mut payload =
            json!({ "title": title, "body": body, "labels": [format!("proj:{project}")] });
        if let Some(kind) = kind_name {
            payload["type"] = json!(kind);
        }
        let (status, resp) = self
            .rest(
                reqwest::Method::POST,
                &format!("/repos/{}/issues", self.repo),
                Some(payload),
            )
            .await?;
        if !status.is_success() {
            return Err(UsecaseError::Board(format!(
                "create issue: HTTP {status}: {}",
                resp.to_string().chars().take(140).collect::<String>()
            )));
        }
        // эхо типа (#1271): GitHub отвечает 422 на неизвестное имя, но
        // «silently dropped» без push-доступа приходит 201 без типа —
        // тихая потеря носителя ловится здесь, а не чтением агента
        if let Some(kind) = kind_name {
            if resp["type"]["name"].as_str() != Some(kind) {
                return Err(UsecaseError::Board(format!(
                    "create issue: тип «{kind}» не принят носителем (эхо: {}) — \
                     словарь issue Type репо или права токена",
                    resp["type"]["name"].as_str().unwrap_or("null")
                )));
            }
        }
        let number = resp["number"].as_u64().unwrap_or_default();
        if number == 0 {
            return Err(UsecaseError::Board("create issue: no number".into()));
        }
        self.add_to_board(board, number).await?;
        Ok(number)
    }

    async fn set_issue_type(&self, number: u64, kind_name: &str) -> UsecaseResult<()> {
        let (status, resp) = self
            .rest(
                reqwest::Method::PATCH,
                &format!("/repos/{}/issues/{number}", self.repo),
                Some(json!({ "type": kind_name })),
            )
            .await?;
        // громкий отказ с телом носителя до сверки эха (#1271): gherr ловит
        // только 403/429 — 422/5xx дошли бы до эхо-ветки с неверным
        // диагнозом вместо точной причины GitHub
        if !status.is_success() {
            return Err(UsecaseError::Board(format!(
                "edit #{number}: HTTP {status}: {}",
                resp.to_string().chars().take(140).collect::<String>()
            )));
        }
        // сверка эха — симметрично create (#1271): тихий дроп без
        // push-доступа не проходит как успех
        if resp["type"]["name"].as_str() != Some(kind_name) {
            return Err(UsecaseError::Board(format!(
                "edit #{number}: тип «{kind_name}» не принят носителем (эхо: {}) — \
                 словарь issue Type репо или права токена",
                resp["type"]["name"].as_str().unwrap_or("null")
            )));
        }
        Ok(())
    }

    async fn issue_fields(&self, number: u64) -> UsecaseResult<uwuwu_domain::TaskFields> {
        let (_, issue) = self
            .rest(
                reqwest::Method::GET,
                &format!("/repos/{}/issues/{number}", self.repo),
                None,
            )
            .await?;
        Ok(map_task_fields(&parse_field_values(&issue)))
    }

    async fn rename_issue(&self, number: u64, title: &str) -> UsecaseResult<()> {
        let (status, _) = self
            .rest(
                reqwest::Method::PATCH,
                &format!("/repos/{}/issues/{number}", self.repo),
                Some(json!({ "title": title })),
            )
            .await?;
        if !status.is_success() {
            return Err(UsecaseError::Board(format!(
                "rename #{number}: HTTP {status}"
            )));
        }
        // тайтл живёт в снапшотах всех досок — сброс целиком (#1235)
        self.snap_cache.invalidate_all();
        Ok(())
    }

    async fn patch_cells(
        &self,
        _number: u64,
        _mutate: uwuwu_usecase::FmMutator,
    ) -> UsecaseResult<()> {
        // fm-ячейки снесены: ветка/PR/приоритет — нативные поля (taskwrite)
        Ok(())
    }

    async fn close_issue_ext(&self, number: u64, not_planned: bool) -> UsecaseResult<()> {
        let reason = if not_planned {
            "not_planned"
        } else {
            "completed"
        };
        let (status, body) = self
            .rest(
                reqwest::Method::PATCH,
                &format!("/repos/{}/issues/{number}", self.repo),
                Some(json!({ "state": "closed", "state_reason": reason })),
            )
            .await?;
        let closed = status.is_success() && body["state"].as_str() == Some("closed");
        if !closed {
            return Err(UsecaseError::Board(format!(
                "close #{number}: false-success PATCH (state={})",
                body["state"]
            )));
        }
        Ok(())
    }

    async fn archive_row(&self, board: &str, number: u64) -> UsecaseResult<()> {
        // item id — из меты снапшота: archiveProjectV2Item ест itemId
        let snap = self.snapshot(board).await?;
        let item = snap.1.items.get(&number).cloned().ok_or_else(|| {
            UsecaseError::Board(format!("row #{number} is not on board «{board}»"))
        })?;
        let project_id = snap.1.project_id.clone();
        let mutation = r#"
          mutation($p:ID!,$i:ID!){
            archiveProjectV2Item(input:{projectId:$p, itemId:$i}){ item{ id isArchived } }
          }"#;
        let data = self
            .gql(mutation, json!({ "p": project_id, "i": item }))
            .await?;
        // payload-поле — item (не projectV2Item; живая проба #1293 поймала)
        let archived = data["archiveProjectV2Item"]["item"]["isArchived"].as_bool();
        if archived != Some(true) {
            return Err(UsecaseError::Board(format!(
                "archive #{number}: false-success (isArchived={:?})",
                archived
            )));
        }
        // Снапшот здесь НЕ инвалидируется: item id из кеша валиден всю
        // волну, а построчный ре-фетч (полный снимок — сотни поинтов на
        // каждую архивацию) выжег бы бюджет первой волной — High-1 ревью
        // #1293. Свежий срез даёт invalidate_snapshot на финале волны.
        Ok(())
    }

    fn gql_budget_remaining(&self) -> Option<u64> {
        Some(self.last_rate_limit.read().expect("rl lock").as_ref()?.0)
    }

    fn invalidate_snapshot(&self, board: &str) {
        self.invalidate_snapshot(board)
    }

    async fn issues_with_title(&self, title: &str) -> UsecaseResult<Vec<u64>> {
        // REST search — вне GraphQL-бюджета (#1293, dup_check create);
        // поиск фразой + клиентский фильтр точного равенства тайтла.
        // percent-encode — реюз вики-энкодера (без новой зависимости,
        // канон github_wiki::encode_path)
        let query = format!("repo:{} in:title \"{}\"", self.repo, title.replace('"', ""));
        let (status, body) = self
            .rest(
                reqwest::Method::GET,
                &format!(
                    "/search/issues?q={}&per_page=100",
                    crate::wiki::encode_path(&query)
                ),
                None,
            )
            .await?;
        if !status.is_success() {
            return Err(UsecaseError::Board(format!("issue search: HTTP {status}")));
        }
        let Some(items) = body["items"].as_array() else {
            return Ok(Vec::new());
        };
        Ok(items
            .iter()
            .filter(|i| i["title"].as_str() == Some(title))
            .filter_map(|i| i["number"].as_u64())
            .collect())
    }

    async fn replace_body(&self, number: u64, body_md: &str) -> UsecaseResult<()> {
        let (status, resp) = self
            .rest(
                reqwest::Method::PATCH,
                &format!("/repos/{}/issues/{number}", self.repo),
                Some(json!({ "body": body_md })),
            )
            .await?;
        let applied = status.is_success() && resp["body"].as_str() == Some(body_md);
        if !applied {
            return Err(UsecaseError::Board(format!(
                "replace body #{number}: not applied (HTTP {status})"
            )));
        }
        Ok(())
    }

    async fn latest_comments(&self, number: u64) -> UsecaseResult<Vec<CommentRef>> {
        self.all_comments(number).await
    }
}

/// Проектный префикс тайтла milestone — `[проект] Название`: у
/// milestones нет лейблов, ассоциация с проектом кодируется в тайтле
/// (#1216). Возвращает (проект, тайтл без префикса); нет префикса —
/// None (такой milestone невидим goal_list ни одного проекта).
fn milestone_project_prefix(title: &str) -> Option<(String, String)> {
    let rest = title.strip_prefix('[')?;
    let close = rest.find(']')?;
    let project = &rest[..close];
    if project.is_empty() {
        return None;
    }
    let name = rest[close + 1..].strip_prefix(' ')?;
    Some((project.to_string(), name.to_string()))
}

/// Цели проекта из ответа GitHub milestones API (#1216): фильтр по
/// проектному префиксу `[проект]` в тайтле, gist — первая
/// содержательная строка description. Чистая функция — юнит-тесты
/// без HTTP.
pub(crate) fn goals_from_milestones(items: &[Value], project: &str) -> Vec<Goal> {
    items
        .iter()
        .filter_map(|m| {
            let (proj, name) = milestone_project_prefix(m["title"].as_str()?)?;
            (proj == project).then(|| Goal {
                number: m["number"].as_u64().unwrap_or(0),
                title: name,
                gist: uwuwu_usecase::gist_of(m["description"].as_str().unwrap_or_default()),
            })
        })
        .collect()
}

#[async_trait]
impl GoalsRepo for GitHubHub {
    /// Открытые цели проекта: milestones hub-репо с префиксом
    /// `[проект]` в тайтле (#1216). REST — вне GraphQL-бюджета (#1243).
    async fn open_goals(&self, project: &str) -> UsecaseResult<Vec<Goal>> {
        let (status, body) = self
            .rest(
                reqwest::Method::GET,
                &format!(
                    "/repos/{}/milestones?state=open&per_page=100",
                    crate::wiki::encode_path(&self.repo)
                ),
                None,
            )
            .await?;
        if !status.is_success() {
            return Err(UsecaseError::Board(format!(
                "goal milestones: HTTP {status}"
            )));
        }
        match body.as_array() {
            Some(items) => Ok(goals_from_milestones(items, project)),
            None => Ok(Vec::new()),
        }
    }
}

#[cfg(test)]
mod goals_json_tests {
    use super::{goals_from_milestones, milestone_project_prefix};
    use serde_json::json;

    #[test]
    fn prefix_parsing_strips_project_and_name() {
        assert_eq!(
            milestone_project_prefix("[origa] Стабилизация"),
            Some(("origa".into(), "Стабилизация".into()))
        );
        assert_eq!(milestone_project_prefix("Без префикса"), None);
        assert_eq!(milestone_project_prefix("[] имя"), None);
        // без пробела после префикса — не по конвенции, невидим
        assert_eq!(milestone_project_prefix("[origa]БезПробела"), None);
    }

    #[test]
    fn maps_prefix_title_and_description_gist() {
        let items = [json!({
            "number": 7,
            "title": "[uwuwu] Переход к единой ИС на Rust",
            "description": "Суть: консолидировать контур.\n\nдетали",
        })];
        let goals = goals_from_milestones(&items, "uwuwu");
        assert_eq!(goals.len(), 1);
        assert_eq!(goals[0].number, 7);
        assert_eq!(goals[0].title, "Переход к единой ИС на Rust");
        assert_eq!(goals[0].gist, "Суть: консолидировать контур.");
    }

    #[test]
    fn filters_by_exact_project_prefix() {
        let items = [
            json!({ "number": 7, "title": "[origa] цель origa", "description": "" }),
            // похожий префикс другого проекта не матчится
            json!({ "number": 8, "title": "[origal] чужая цель", "description": "" }),
            json!({ "number": 9, "title": "без префикса", "description": "" }),
        ];
        let goals = goals_from_milestones(&items, "origa");
        assert_eq!(goals.len(), 1);
        assert_eq!(goals[0].number, 7);
        assert_eq!(goals[0].title, "цель origa");
    }

    #[test]
    fn empty_or_blank_description_yields_empty_gist() {
        let items = [
            json!({ "number": 60, "title": "[uwuwu] без тела", "description": "" }),
            json!({ "number": 61, "title": "[uwuwu] null-тело", "description": null }),
            json!({ "number": 62, "title": "[uwuwu] пробельное", "description": "  \n\t " }),
        ];
        let goals = goals_from_milestones(&items, "uwuwu");
        assert_eq!(goals.len(), 3);
        assert!(goals.iter().all(|g| g.gist.is_empty()));
    }

    #[test]
    fn empty_array_yields_empty_list() {
        assert!(goals_from_milestones(&[], "uwuwu").is_empty());
    }
}

#[cfg(test)]
mod http_timeout_pin_tests {
    use super::HTTP_TIMEOUT;
    use std::time::Duration;

    /// Пин таймаута (#1502): инцидент 22.11 — клиент без таймаута при
    /// зависшем GitHub-запросе парализовал tick_loop навсегда. Гвард
    /// против выпила/безмолвного роста: значение обязано быть > 0,
    /// перекрывать легитимный снапшот (~40 c) и оставаться ниже poll_sec.
    #[test]
    fn http_timeout_is_pinned() {
        assert_eq!(HTTP_TIMEOUT, Duration::from_secs(120));
    }
}

#[cfg(test)]
mod goals_live_tests {
    use super::GitHubHub;
    use uwuwu_usecase::GoalsRepo;

    // Живой e2e REST-границы целей (#1216): реальный hub-репо, гвард
    // UWUWU_E2E=1 (культура репо — образец gitea_tests::live_probe_*).
    // Ловит возврат «слепого» потребителя: поломку запроса (state),
    // маппинг ответа и проектный фильтр префикса на живых данных.
    // Фиксация на живой реестр uwuwu (milestones #1/#2): если владелец
    // закрыл все цели — тест правится на актуальный факт, не сносится.
    #[tokio::test]
    async fn live_goal_list_reads_open_milestones_by_prefix() {
        if std::env::var("UWUWU_E2E").as_deref() != Ok("1") {
            return;
        }
        let token = std::env::var("GH_TOKEN").unwrap_or_default();
        assert!(!token.is_empty(), "UWUWU_E2E=1: нужен GH_TOKEN");
        let hub = GitHubHub::new("yurvon-screamo/uwuwu-hub".into(), token);
        let goals = hub
            .open_goals("uwuwu")
            .await
            .expect("живой REST open_goals uwuwu");
        assert!(
            !goals.is_empty(),
            "реестр uwuwu пуст на живом hub — сломан носитель или запрос"
        );
        for g in &goals {
            assert!(g.number > 0, "номер milestone не смапился: {g:?}");
            assert!(!g.title.is_empty(), "тайтл не смапился: {g:?}");
        }
        // проектный фильтр префикса: несуществующий проект — пусто, не чужие цели
        let none = hub
            .open_goals("no-such-proj-1216")
            .await
            .expect("живой REST open_goals по несуществующему проекту");
        assert!(
            none.is_empty(),
            "префикс-фильтр пропустил чужие цели: {none:?}"
        );
    }
}

/// Чистка доски (#1158): свежие пробы мимо кэшей + REST-мутации сноса/
/// восстановления (ADR-113 — Pv2 REST, развод квот). Вердикты юзкейса
/// стоят на этих пробах — недостоверная проба = Divergent = не трогать.
#[async_trait]
impl uwuwu_usecase::CleanBoardRepo for GitHubHub {
    async fn fresh_index(
        &self,
        board: &str,
    ) -> UsecaseResult<HashMap<u64, uwuwu_usecase::RowBinding>> {
        Ok(self
            .pv2
            .fresh_index(board)
            .await?
            .into_iter()
            .map(|(number, item)| {
                (
                    number,
                    uwuwu_usecase::RowBinding {
                        item_id: item.id,
                        status_raw: item.status.filter(|s| !s.trim().is_empty()),
                    },
                )
            })
            .collect())
    }

    async fn fresh_issue(&self, number: u64) -> UsecaseResult<Option<uwuwu_usecase::IssueProbe>> {
        let (status, issue) = self
            .rest(
                reqwest::Method::GET,
                &format!("/repos/{}/issues/{number}", self.repo),
                None,
            )
            .await?;
        // 404 — issue нет (снесён вручную): None для вердикта Divergent
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(UsecaseError::Board(format!(
                "issue probe #{number}: HTTP {status}"
            )));
        }
        Ok(Some(uwuwu_usecase::IssueProbe {
            title: issue["title"].as_str().unwrap_or_default().to_string(),
            open: issue["state"].as_str() == Some("open"),
            body: issue["body"].as_str().unwrap_or_default().to_string(),
        }))
    }

    async fn remove_row(&self, board: &str, number: u64, item_id: u64) -> UsecaseResult<()> {
        self.pv2.delete_item(board, number, item_id).await?;
        // снапшот GraphQL-вью тоже отдаёт строку (кэш 240 с) — сброс:
        // следующий read_board видит состав после сноса
        self.invalidate_snapshot(board);
        Ok(())
    }

    async fn restore_row(
        &self,
        board: &str,
        number: u64,
        status_raw: Option<&str>,
    ) -> UsecaseResult<u64> {
        // add без дефолта Analys (в отличие от create_issue): undo возвращает
        // статус из журнала, не дефолт create-пути
        let item_id = self.pv2.add_item(board, number).await?;
        if let Some(raw) = status_raw.filter(|s| !s.trim().is_empty()) {
            self.pv2.set_status(board, number, raw).await?;
        }
        self.invalidate_snapshot(board);
        Ok(item_id)
    }
}

/// GraphQL-носитель каталога Status-поля и rename-мутации (#1447):
/// REST ProjectsV2 полей не мутирует (List/Add/Get — docs.github.com
/// 22.11), живой прецедент GraphQL-Pv2 мимо ADR-113 — archive_row.
/// Контракт формы options — живая проба sandbox-доски (срез 2 #1447).
const STATUS_FIELD_QUERY: &str = r#"
  query($owner:String!,$name:String!,$board:String!){
    rateLimit{ remaining resetAt }
    repository(owner:$owner,name:$name){
      projectsV2(first:20,query:$board){
        nodes{ id title
          field(name:"Status"){ ...on ProjectV2SingleSelectField{ id options{ id name color description } } }
        }
      }
    }
  }"#;

const STATUS_RENAME_MUTATION: &str = r#"
  mutation($field:ID!,$options:[ProjectV2SingleSelectFieldOptionInput!]){
    updateProjectV2Field(input:{fieldId:$field, singleSelectOptions:$options}){
      projectV2Field{ ...on ProjectV2SingleSelectField{ id options{ id name color description } } }
    }
  }"#;

/// Опции из gql-ответа (id/name обязательны; color/description —
/// непустые строки, пустые → None). Строгий парс: запись без id/name —
/// громкий отказ, НЕ тихий срез (усечённый pre-image → усечённый
/// «дословный» payload → replace сносит выброшенные опции — тот механизм,
/// против которого построена операция; ревью #1447 Common-3).
fn parse_status_options(
    raw: &serde_json::Value,
) -> UsecaseResult<Vec<uwuwu_usecase::StatusOption>> {
    let list = raw
        .as_array()
        .ok_or_else(|| UsecaseError::Board("Status field: options array missing".into()))?;
    let mut out = Vec::with_capacity(list.len());
    for o in list {
        let id = o["id"].as_str().unwrap_or_default();
        let name = o["name"].as_str().unwrap_or_default();
        if id.is_empty() || name.is_empty() {
            return Err(UsecaseError::Board(format!(
                "Status option entry without id/name in carrier response \
                 (truncated pre-read?): {:} entries read, offender: {}",
                out.len(),
                serde_json::to_string(o).unwrap_or_default()
            )));
        }
        let s = |v: &serde_json::Value| v.as_str().map(str::to_string).filter(|x| !x.is_empty());
        out.push(uwuwu_usecase::StatusOption {
            id: id.to_string(),
            name: name.to_string(),
            color: s(&o["color"]),
            description: s(&o["description"]),
        });
    }
    Ok(out)
}

#[async_trait]
impl uwuwu_usecase::BoardStatusRepo for GitHubHub {
    async fn fresh_field(&self, board: &str) -> UsecaseResult<uwuwu_usecase::StatusFieldImage> {
        let (owner, name) = self
            .repo
            .split_once('/')
            .ok_or_else(|| UsecaseError::Board(format!("repo: {}", self.repo)))?;
        let data = self
            .gql(
                STATUS_FIELD_QUERY,
                json!({ "owner": owner, "name": name, "board": board }),
            )
            .await?;
        let project = data["repository"]["projectsV2"]["nodes"]
            .as_array()
            .and_then(|nodes| nodes.iter().find(|p| p["title"] == board))
            .ok_or_else(|| {
                UsecaseError::Board(format!(
                    "project \"{board}\" not found in projectsV2 {owner}"
                ))
            })?;
        let field = &project["field"];
        let field_node_id = field["id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                UsecaseError::Board(format!("board \"{board}\": Status field has no node id"))
            })?
            .to_string();
        let options = parse_status_options(&field["options"])?;
        if options.is_empty() {
            return Err(UsecaseError::Board(format!(
                "board \"{board}\": Status field has no options"
            )));
        }
        Ok(uwuwu_usecase::StatusFieldImage {
            field_node_id,
            options,
        })
    }

    async fn fresh_distribution(&self, board: &str) -> UsecaseResult<HashMap<u64, Option<String>>> {
        Ok(self
            .pv2
            .fresh_index(board)
            .await?
            .into_iter()
            .map(|(number, item)| (number, item.status))
            .collect())
    }

    async fn rename(
        &self,
        image: &uwuwu_usecase::StatusFieldImage,
        option_id: &str,
        new_name: &str,
    ) -> UsecaseResult<Vec<uwuwu_usecase::StatusOption>> {
        // дословный pre-image набор: id/color/description сохранены,
        // изменено ровно одно имя (набор операцией не конструируется).
        // Живая проба 22.11 (#1447, sandbox): ProjectV2SingleSelectFieldOptionInput
        // требует color и description (non-null) — поля присутствуют всегда;
        // пустой description носителя → "", color опций без явного — дефолт GRAY
        let options: Vec<serde_json::Value> = image
            .options
            .iter()
            .map(|o| {
                let name = if o.id == option_id {
                    new_name
                } else {
                    o.name.as_str()
                };
                json!({
                    "id": o.id,
                    "name": name,
                    "color": o.color.clone().unwrap_or_else(|| "GRAY".into()),
                    "description": o.description.clone().unwrap_or_default(),
                })
            })
            .collect();
        let data = self
            .gql(
                STATUS_RENAME_MUTATION,
                json!({
                    "field": image.field_node_id,
                    "options": options
                }),
            )
            .await?;
        let payload =
            parse_status_options(&data["updateProjectV2Field"]["projectV2Field"]["options"])?;
        if payload.is_empty() {
            return Err(UsecaseError::Board(
                "rename: carrier returned no options in mutation payload (false-success?)".into(),
            ));
        }
        Ok(payload)
    }

    async fn verify_catalog(&self, board: &str) -> UsecaseResult<Vec<(String, String)>> {
        // каждое контрольное чтение — свежий фетч носителя: окно ретраев
        // verify обязано видеть живой каталог, а не собственный снимок
        // первой попытки (ревью #1447 раунд 2 Common-1: meta TTL 600с
        // замораживал окно — попытки 2-3 были localhost-хитами)
        self.pv2.invalidate_meta(board);
        let meta = self.pv2.meta(board).await?;
        Ok(meta
            .options
            .iter()
            .map(|(name, id)| (id.clone(), name.clone()))
            .collect())
    }

    fn invalidate_meta(&self, board: &str) {
        self.pv2.invalidate_meta(board);
    }
}

impl GitHubHub {
    /// Поля задачи (нативные): лениво для строк-кандидатов тика.
    #[allow(dead_code)] // точка входа ленивых чтений тика (срез анти-шторма подключит)
    pub async fn load_fields(&self, number: u64) -> UsecaseResult<Fields> {
        self.fields.load(number).await
    }

    async fn add_to_board(&self, board: &str, number: u64) -> UsecaseResult<()> {
        // add + дефолт Analys (#1135) — REST ProjectsV2 (#1236): POST
        // принимает owner/repo/number, GraphQL node_id issue не нужен;
        // опция ставится по имени колонки; колонки нет — строка остаётся
        // без статуса (rollout плана)
        self.pv2.add_item(board, number).await?;
        if self.pv2.meta(board).await?.options.contains_key("Analys") {
            self.pv2.set_status(board, number, "Analys").await?;
        }
        self.invalidate_snapshot(board);
        Ok(())
    }
}

/// Источник задач для семантического индекса: последние issues репо
/// (state=all, новейшие первыми), тело — как есть.
#[async_trait]
impl uwuwu_usecase::IssueIndexSource for GitHubHub {
    async fn recent_issues(
        &self,
        limit: usize,
    ) -> uwuwu_usecase::UsecaseResult<Vec<uwuwu_usecase::IssueDigest>> {
        let mut out = Vec::new();
        let mut page = 1u32;
        while out.len() < limit && page <= 5 {
            let (status, body) = self
                .rest(
                    reqwest::Method::GET,
                    &format!(
                        "/repos/{}/issues?state=all&per_page=100&page={page}",
                        self.repo
                    ),
                    None,
                )
                .await?;
            if !status.is_success() {
                return Err(UsecaseError::Board(format!("recent issues: HTTP {status}")));
            }
            let list = body.as_array().cloned().unwrap_or_default();
            if list.is_empty() {
                break;
            }
            for i in list {
                // PR тоже issues в этом эндпоинте — фильтруем
                if i["pull_request"].is_object() {
                    continue;
                }
                let number = i["number"].as_u64().unwrap_or_default();
                if number == 0 {
                    continue;
                }
                out.push(uwuwu_usecase::IssueDigest {
                    number,
                    title: i["title"].as_str().unwrap_or_default().to_string(),
                    body: i["body"].as_str().unwrap_or_default().to_string(),
                });
                if out.len() >= limit {
                    break;
                }
            }
            page += 1;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod board_status_live_tests {
    //! Живой e2e rename статуса (#1447): реальный носитель, гвард
    //! UWUWU_E2E=1 (образец — goals_live_tests). Ловит дрейф контракта
    //! GraphQL-мутации/чтений (то, что MemoryHub-пины поймать не могут):
    //! fresh_field парсит живой каталог, rename уходит по option-id с
    //! дословным оформлением, post-verify проходит на живом носителе,
    //! значение draft-строки следует за rename (GraphQL-проверка —
    //! draft-строки вне issue-number индекса дистрибуции, живой факт
    //! ручного прогона #1447). Артефакты (sandbox-доска, draft-строка,
    //! temp-журнал) сносятся рамками прогона; заголовок доски несёт маркер
    //! live-e2e-1447 — протечка при панике в setup заметна владельцу.

    use super::GitHubHub;
    use crate::clock::SystemClock;
    use crate::rename_journal::JsonStatusPreImages;
    use serde_json::{json, Value};
    use std::sync::Arc;
    use uwuwu_usecase::Clock as _;
    use uwuwu_usecase::{BoardStatusRenameUseCase, BoardStatusRepo};

    const OWNER: &str = "yurvon-screamo";
    const REPO: &str = "yurvon-screamo/uwuwu-hub";
    const FROM: &str = "In Progress";
    const TO: &str = "Вейт"; // канон-статус (parse_status → Wait) — warn-ветки не задеваем

    /// Строка контрольного каталога: (id, name, color, description).
    type CatalogRow = (String, String, Option<String>, Option<String>);

    /// Нормализация оформления к «отсутствие»: пустая строка носителя
    /// == null. Обе стороны сверки проходят одну нормализацию — без неё
    /// pre-сторона Some("") → None, а post-сторона осталась бы Some("")
    /// (ложный красный, тест-ревью 1).
    fn norm(s: Option<&str>) -> Option<&str> {
        s.filter(|s| !s.is_empty())
    }

    /// Лучшее усилие мутации setup/teardown: тест-инфраструктура,
    /// падение teardown — не ложный красный фичи (артефакт уйдёт
    /// владельцу по маркеру live-e2e-1447). Ошибка носителя возвращается
    /// текстом — expect в месте вызова кладёт её в панику (диагностика
    /// setup/teardown, тест-ревью 1), best-effort сохраняют `let _ =`.
    async fn raw_gql(hub: &GitHubHub, query: &str, vars: Value) -> Result<Value, String> {
        hub.gql(query, vars)
            .await
            .map_err(|e| e.to_string())
            .map(|mut data| {
                // один корень data — отдаём его содержимое
                data.as_object_mut()
                    .map(|m| Value::Object(m.clone()))
                    .unwrap_or(Value::Null)
            })
    }

    #[tokio::test]
    async fn live_board_status_rename_by_id_values_follow() {
        if std::env::var("UWUWU_E2E").as_deref() != Ok("1") {
            return;
        }
        let token = std::env::var("GH_TOKEN").unwrap_or_default();
        assert!(!token.is_empty(), "UWUWU_E2E=1: нужен GH_TOKEN");
        let hub = Arc::new(GitHubHub::new(REPO.into(), token));
        let ts = SystemClock.now_ms();
        let board_title = format!("Задачи live-e2e-1447-{ts}");

        // --- setup: sandbox-доска, линк к репо (fresh_field ищет через
        // repository.projectsV2 — нелинкованная свежая доска невидима,
        // живой факт ручного прогона #1447), draft-строка со статусом.
        let owner_id = raw_gql(
            &hub,
            "query($login:String!){user(login:$login){id}}",
            json!({ "login": OWNER }),
        )
        .await
        .expect("живой носитель: ownerId")[&"user"][&"id"]
            .as_str()
            .expect("ownerId")
            .to_string();
        let project_id = raw_gql(
            &hub,
            "mutation($ownerId:ID!,$title:String!){createProjectV2(input:{ownerId:$ownerId,\
             title:$title}){projectV2{id}}}",
            json!({ "ownerId": owner_id, "title": board_title }),
        )
        .await
        .expect("живой носитель: createProjectV2")[&"createProjectV2"][&"projectV2"][&"id"]
            .as_str()
            .expect("project id")
            .to_string();
        let repo_id = raw_gql(
            &hub,
            &format!("query{{repository(owner:\"{OWNER}\",name:\"uwuwu-hub\"){{id}}}}"),
            json!({}),
        )
        .await
        .expect("живой носитель: repoId")[&"repository"][&"id"]
            .as_str()
            .expect("repo id")
            .to_string();
        raw_gql(
            &hub,
            "mutation($projectId:ID!,$repositoryId:ID!){linkProjectV2ToRepository(input:{\
             projectId:$projectId,repositoryId:$repositoryId}){repository{id}}}",
            json!({ "projectId": project_id, "repositoryId": repo_id }),
        )
        .await
        .expect("линк sandbox-доски к репо");
        let field = raw_gql(
            &hub,
            "query($projectId:ID!){node(id:$projectId){... on ProjectV2{field(name:\"Status\"){\
             ... on ProjectV2SingleSelectField{id options{id name color description}}}}}}",
            json!({ "projectId": project_id }),
        )
        .await
        .expect("дефолт Status-поле новой доски")[&"node"][&"field"]
            .clone();
        let options = field[&"options"].as_array().expect("опции Status").clone();
        assert!(
            options.iter().any(|o| o[&"name"] == FROM),
            "дефолт-набор без «{FROM}» — тест правится на актуальный дефолт носителя"
        );
        let field_id = field[&"id"].as_str().expect("field node id").to_string();
        let from_option_id = options
            .iter()
            .find(|o| o[&"name"].as_str() == Some(FROM))
            .and_then(|o| o[&"id"].as_str())
            .expect("option id from")
            .to_string();
        let item_id = raw_gql(
            &hub,
            "mutation($projectId:ID!){addProjectV2DraftIssue(input:{projectId:$projectId,\
             title:\"live-e2e-1447 row\"}){projectItem{id}}}",
            json!({ "projectId": project_id }),
        )
        .await
        .expect("draft-строка")[&"addProjectV2DraftIssue"][&"projectItem"][&"id"]
            .as_str()
            .expect("item id")
            .to_string();
        raw_gql(
            &hub,
            "mutation($projectId:ID!,$itemId:ID!,$fieldId:ID!,$optionId:String!){\
             updateProjectV2ItemFieldValue(input:{projectId:$projectId,itemId:$itemId,\
             fieldId:$fieldId,value:{singleSelectOptionId:$optionId}}){projectV2Item{id}}}",
            json!({
                "projectId": project_id,
                "itemId": item_id,
                "fieldId": field_id,
                "optionId": from_option_id
            }),
        )
        .await
        .expect("статус draft-строки");

        // --- поток операции: usecase на живом адаптере (тот же объект,
        // что за MCP-тулом; arm-пины — tools_pins_tests на MemoryHub).
        let journal_path = std::env::temp_dir().join(format!("uwuwu-1447-live-{ts}.json"));
        let usecase = BoardStatusRenameUseCase {
            repo: hub.clone() as Arc<dyn BoardStatusRepo>,
            journal: Arc::new(JsonStatusPreImages::load(journal_path.clone())),
            boards: vec![("live-e2e".into(), board_title.clone())],
            clock: Arc::new(SystemClock),
        };
        let report = usecase
            .rename("live-e2e", FROM, TO)
            .await
            .expect("живой rename по option-id");
        assert_eq!(report.option_id, from_option_id, "id опции сохранён");
        assert_eq!(report.options_total, options.len());
        assert!(
            report.warnings.is_empty(),
            "канон→канон без warn: {:?}",
            report.warnings
        );
        assert!(
            report.histogram_after.contains_key(TO),
            "гистограмма после: новое имя в каталоге"
        );

        // write-ahead журнал: verbatim pre-image до мутации
        let journal: Value =
            serde_json::from_str(&std::fs::read_to_string(&journal_path).expect("журнал"))
                .expect("журнал парсится");
        let stored = &journal[&"by_board"][board_title.as_str()];
        assert_eq!(stored[&"from"], json!(FROM), "журнал: from");
        assert_eq!(stored[&"to"], json!(TO), "журнал: to");
        let stored_options = stored[&"options"].as_array().expect("журнал: опции");
        assert_eq!(stored_options.len(), options.len(), "журнал: полный набор");
        for o in &options {
            let found = stored_options
                .iter()
                .find(|s| s[&"id"] == o[&"id"])
                .expect("журнал: опция из pre-image");
            assert_eq!(found[&"color"], o[&"color"], "журнал: color дословно");
            assert_eq!(
                found[&"description"], o[&"description"],
                "журнал: описание дословно"
            );
        }
        // draft-строки вне issue-number индекса дистрибуции (живой факт
        // #1447) — распределение здесь пусто, значение строки проверено ниже по GraphQL
        assert_eq!(
            stored[&"distribution"].as_object().map(|m| m.len()),
            Some(0),
            "журнал: только draft-строки — их нет в дистрибуции"
        );

        // --- контрольный перечит носителя мимо операции: id-набор цел,
        // оформление дословно, имя сменилось у ровно одной опции (read-лаг
        // носителя — окно ретраев; структурное расхождение — красный тест).
        let hub_repo = hub.clone();
        let mut post: Option<Vec<CatalogRow>> = None;
        for _ in 0..10 {
            match hub_repo.fresh_field(&board_title).await {
                Ok(image) => {
                    let got: Vec<_> = image
                        .options
                        .iter()
                        .map(|o| {
                            (
                                o.id.clone(),
                                o.name.clone(),
                                o.color.clone(),
                                o.description.clone(),
                            )
                        })
                        .collect();
                    let renamed = got
                        .iter()
                        .find(|(id, ..)| *id == from_option_id)
                        .map(|(_, name, ..)| name.clone());
                    if renamed.as_deref() == Some(TO) {
                        post = Some(got);
                        break;
                    }
                }
                Err(e) => panic!("fresh_field после rename: {e}"),
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
        let post = post.expect("каталог не подтвердил новое имя за окно ретраев");
        assert_eq!(post.len(), options.len(), "набор опций сжался/расширился");
        for (id, name, color, description) in &post {
            let pre = options
                .iter()
                .find(|o| o[&"id"].as_str() == Some(id.as_str()))
                .expect("чужая опция в каталоге после rename");
            assert_eq!(
                norm(color.as_deref()),
                norm(pre[&"color"].as_str()),
                "color опции {name} изменён"
            );
            assert_eq!(
                norm(description.as_deref()),
                norm(pre[&"description"].as_str()),
                "description опции {name} изменён"
            );
            let expected_name = if id == &from_option_id {
                TO
            } else {
                pre[&"name"].as_str().expect("имя")
            };
            assert_eq!(name, expected_name, "имя опции после rename");
        }

        // значение draft-строки следует за rename (option-id семантика)
        let values = raw_gql(
            &hub,
            "query($itemId:ID!){node(id:$itemId){... on ProjectV2Item{fieldValues(first:10){\
             nodes{... on ProjectV2ItemFieldSingleSelectValue{name}}}}}}",
            json!({ "itemId": item_id }),
        )
        .await
        .expect("fieldValues draft-строки")[&"node"][&"fieldValues"][&"nodes"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let value_name = values
            .iter()
            .filter_map(|n| n[&"name"].as_str())
            .find(|n| *n == FROM || *n == TO)
            .expect("строка потеряла Status-значение");
        assert_eq!(value_name, TO, "значение строки следует за renamed опцией");

        // --- teardown: draft-сироты и доска сносятся (журнал — temp, за собой).
        let _ = raw_gql(
            &hub,
            "mutation($projectId:ID!,$itemId:ID!){deleteProjectV2Item(input:{projectId:$projectId,\
             itemId:$itemId}){clientMutationId}}",
            json!({ "projectId": project_id, "itemId": item_id }),
        )
        .await;
        let _ = raw_gql(
            &hub,
            "mutation($projectId:ID!){deleteProjectV2(input:{projectId:$projectId}){\
             clientMutationId}}",
            json!({ "projectId": project_id }),
        )
        .await;
        let _ = std::fs::remove_file(&journal_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uwuwu_domain::TaskType;
    use uwuwu_domain::{gate, Gate, PeakWindow, Priority, SpawnPolicy, UtcMinute};

    #[test]
    fn issue_backed_priority_reaches_peak_gate() {
        // #1174 (потребительский чекпоинт ревью плана): приоритет, прочитанный
        // с issue-backed ячейки снапшота, доезжает до пик-гейта — до фикса
        // снапшот всегда был пуст и high-строки деферились (инцидент #1170)
        let item = json!({
            "id": "IT_x",
            "content": { "__typename": "Issue", "number": 1, "title": "t", "state": "open" },
            "fieldValues": { "nodes": [
                { "__typename": "ProjectV2ItemIssueFieldValue",
                  "field": { "__typename": "ProjectV2SingleSelectField", "name": "Приоритет" },
                  "issueFieldValue": { "__typename": "IssueFieldSingleSelectValue",
                                       "name": "high", "value": "high", "optionId": "IFSSO_x" } },
            ]}
        });
        let mut meta = BoardMeta::default();
        let task = &parse_page(&[item], &mut meta)[0];
        assert_eq!(task.priority(), Priority::High);

        let policy = SpawnPolicy {
            cup: 2,
            peak: PeakWindow {
                enabled: true,
                start_min: 360,
                end_min: 600,
            },
        };
        let in_peak = UtcMinute {
            minute_of_day: 540,
            weekday: 3,
        };
        let spawn = uwuwu_domain::decide(task);
        assert_eq!(
            gate(task, &spawn, policy, 0, in_peak),
            Gate::Allowed,
            "high с снапшота проходит пик-окно"
        );
    }

    /// Фикстура: снапшот с устаревшей строкой + write-through нового
    /// статуса — состояние set_status сразу после мутации, до verify.
    fn hub_after_write_through(board: &str, number: u64, status: Status) -> GitHubHub {
        let hub = GitHubHub::new("o/r".into(), "t".into());
        let now = std::time::Instant::now();
        let rows = vec![Task::new(number, "строка".into(), Status::Analys)];
        hub.snap_cache.put(board, (rows, BoardMeta::default()), now);
        hub.write_through_status(board, number, status);
        hub.write_through_status_cache(number, status);
        hub
    }

    #[test]
    fn settle_ok_keeps_write_through() {
        let hub = hub_after_write_through("B", 7, Status::InProgress);
        hub.settle_write_through("B", 7, VerifyOutcome::Ok, Status::InProgress)
            .expect("Ok — кэш не трогается");
        let snap = hub
            .snap_cache
            .get("B", std::time::Instant::now())
            .expect("снапшот жив");
        assert_eq!(
            snap.0.iter().find(|t| t.number() == 7).unwrap().status(),
            Status::InProgress
        );
        assert_eq!(
            hub.status_cache
                .get("7", std::time::Instant::now())
                .as_deref(),
            Some(&"In Progress".to_string())
        );
    }

    #[test]
    fn settle_rate_limit_skip_keeps_write_through() {
        // RATE_LIMITED: write-through сохраняем — окно возможно-ложного
        // значения ограничено TTL, шуметь ошибкой на бюджетный отказ не надо
        let hub = hub_after_write_through("B", 7, Status::InProgress);
        hub.settle_write_through(
            "B",
            7,
            VerifyOutcome::SkippedRateLimited,
            Status::InProgress,
        )
        .expect("skip — не ошибка");
        assert!(
            hub.snap_cache.get("B", std::time::Instant::now()).is_some(),
            "write-through сохранён"
        );
    }

    #[test]
    fn settle_mismatch_rolls_back_write_through_and_errors() {
        // Деликатная ветка: «success без эффекта» не должен держать ложный
        // статус в кэше 60–120 с (#1213-семья, код-ревью Common-3)
        let hub = hub_after_write_through("B", 7, Status::InProgress);
        let err = hub
            .settle_write_through("B", 7, VerifyOutcome::Mismatch, Status::InProgress)
            .expect_err("mismatch — громкая ошибка");
        assert!(
            err.to_string().contains("not confirmed by read"),
            "текст отказа: {err}"
        );
        assert!(
            hub.snap_cache.get("B", std::time::Instant::now()).is_none(),
            "снапшот откатан"
        );
        assert!(
            hub.status_cache
                .get("7", std::time::Instant::now())
                .is_none(),
            "статусный кэш откатан"
        );
    }

    /// #1258: write-through нестатусных ячеек — записанные Приоритет/
    /// Automerge видны кешированному снапшоту сразу (read-your-writes,
    /// симметрия write_through_status).
    #[test]
    fn board_cells_write_through_updates_snapshot() {
        let hub = GitHubHub::new("o/r".into(), "t".into());
        hub.snap_cache.put(
            "B",
            (
                vec![Task::new(7, "строка".into(), Status::Analys)],
                BoardMeta::default(),
            ),
            std::time::Instant::now(),
        );
        hub.settle_board_cells("B", 7, Some("high"), Some(true), None);
        let snap = hub
            .snap_cache
            .get("B", std::time::Instant::now())
            .expect("снапшот жив");
        let t = snap
            .0
            .iter()
            .find(|t| t.number() == 7)
            .expect("строка в снапшоте");
        assert_eq!(t.priority(), Priority::High, "приоритет write-through'нут");
        assert!(t.automerge(), "automerge write-through'нут");
    }

    /// #1258: строки нет в снимке (окно create) — снимок инвалидируется,
    /// следующий фетч видит поля; кеш без строки не держится.
    #[test]
    fn board_cells_missing_row_invalidates_snapshot() {
        let hub = GitHubHub::new("o/r".into(), "t".into());
        hub.snap_cache.put(
            "B",
            (
                vec![Task::new(8, "другая строка".into(), Status::Analys)],
                BoardMeta::default(),
            ),
            std::time::Instant::now(),
        );
        hub.settle_board_cells("B", 7, Some("high"), None, None);
        assert!(
            hub.snap_cache.get("B", std::time::Instant::now()).is_none(),
            "снимок без строки сброшен"
        );
    }

    /// #1258: пустой кеш — чистый no-op (инвалидация мимо, без паники).
    #[test]
    fn board_cells_without_snapshot_is_clean_noop() {
        let hub = GitHubHub::new("o/r".into(), "t".into());
        hub.settle_board_cells("B", 7, Some("high"), None, None);
        assert!(hub.snap_cache.get("B", std::time::Instant::now()).is_none());
    }

    /// #1258 живая проба (tester): read-your-writes нестатусных ячеек
    /// против реального носителя. МУТАЦИОННАЯ — пишет и восстанавливает;
    /// носитель — sandbox #1265 (носитель живых проб #1232/#1174).
    /// До фикса: set_board_cells мимо snap_cache → немедленный read_board
    /// горячего хаба показывал до-мутационное значение до TTL (проба
    /// на проде 18.11). Контракт под проверку: запись → немедленное
    /// чтение тем же хабом несёт записанное (write-through снапшота);
    /// свежий хаб — раунд-трип REST-записи на носителе. Автомерж на
    /// свежем чтении ассертится (Pv2-отражение стабильно, NB #1232);
    /// приоритет — informational-принт: Pv2-отражение issue-backed
    /// Приоритета под задокументированным дрейфом GitHub (#1232).
    #[test]
    #[ignore] // живой прогон: GH_TOKEN + сеть; MUTATION — пишет и восстанавливает
    fn live_board_cells_write_through_on_real_carrier() {
        let repo =
            std::env::var("UWUWU_HUB_REPO").unwrap_or_else(|_| "yurvon-screamo/uwuwu-hub".into());
        let token = std::env::var("GH_TOKEN").expect("GH_TOKEN");
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async {
            let board = "Задачи sandbox";
            let n = 1265u64;
            // 1. базлайн: холодный хаб наполняет кеш живым фетчем
            let hub = GitHubHub::new(repo.clone(), token.clone());
            let base = hub.read_board(board).await.expect("read_board базлайн");
            let row = base
                .iter()
                .find(|t| t.number() == n)
                .expect("#1265 на доске sandbox");
            let base_priority = row.priority();
            let base_automerge = row.automerge();
            // проба пишет значение, ОТЛИЧНОЕ от базлайна — ассерты значимы
            // при любом состоянии носителя
            let probe_priority: &str = match base_priority {
                Priority::High | Priority::Medium => "low",
                Priority::Low | Priority::Empty => "high",
            };
            let probe_automerge = !base_automerge;
            // 2. контракт фикса: запись → СРАЗУ чтение тем же хабом
            hub.set_board_cells(board, n, Some(probe_priority), Some(probe_automerge), None)
                .await
                .expect("set_board_cells");
            let after = hub
                .read_board(board)
                .await
                .expect("read_board сразу после записи");
            let row2 = after
                .iter()
                .find(|t| t.number() == n)
                .expect("#1265 жив после записи");
            assert_eq!(
                row2.priority(),
                uwuwu_usecase::parse_priority(probe_priority),
                "read-your-writes: приоритет виден сразу (write-through снапшота, #1258)"
            );
            assert_eq!(
                row2.automerge(),
                probe_automerge,
                "read-your-writes: automerge виден сразу (write-through снапшота, #1258)"
            );
            // 3. свежий хаб (холодный кеш): раунд-трип записи на носителе;
            // до 3 попыток с паузой (Pv2-отражение REST-записи — секундное)
            let mut roundtrip = false;
            for attempt in 0..3 {
                let fresh = GitHubHub::new(repo.clone(), token.clone());
                let rows = fresh.read_board(board).await.expect("read_board носителя");
                let matched = rows.iter().find(|t| t.number() == n).map(|t| {
                    println!(
                        "инфо: свежий хаб, попытка {}: приоритет = {:?} (Pv2-отражение \
                         issue-backed Приоритета под дрейфом #1232 — не ассерт)",
                        attempt + 1,
                        t.priority()
                    );
                    t.automerge() == probe_automerge
                });
                if matched == Some(true) {
                    roundtrip = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
            assert!(
                roundtrip,
                "носитель не отдал записанный automerge за 3 попытки (раунд-трип REST #1258)"
            );
            // 4. восстановление базлайна + контроль тем же хабом
            let base_priority_arg: &str = match base_priority {
                Priority::High => "high",
                Priority::Medium => "medium",
                Priority::Low => "low",
                Priority::Empty => "", // очистка ячейки
            };
            hub.set_board_cells(
                board,
                n,
                Some(base_priority_arg),
                Some(base_automerge),
                None,
            )
            .await
            .expect("восстановление базлайна");
            let restored = hub
                .read_board(board)
                .await
                .expect("read_board после отката");
            let row4 = restored
                .iter()
                .find(|t| t.number() == n)
                .expect("#1265 жив после отката");
            assert_eq!(
                row4.priority(),
                base_priority,
                "базлайн приоритета восстановлен"
            );
            assert_eq!(
                row4.automerge(),
                base_automerge,
                "базлайн automerge восстановлен"
            );
        });
    }

    #[test]
    fn is_rate_limited_matches_only_owned_tag_not_github_texts() {
        // Permission-отказ: Display содержит «не rate limit» — старый
        // строковый матч читал его как rate limit (код-ревью Common-1)
        let permission = UsecaseError::Board(
            "gh graphql [PERMISSION]: HTTP 403 (not rate limit): check GH_TOKEN / the token's permissions"
                .into(),
        );
        assert!(!is_rate_limited(&permission), "Permission — не rate limit");
        let limited = UsecaseError::Board(
            "gh graphql [RATE_LIMITED]: GitHub rate limit (core): the quota is exhausted".into(),
        );
        assert!(is_rate_limited(&limited));
        let backoff =
            UsecaseError::Board("gh graphql [RATE_LIMITED]: backoff for another 4 min".into());
        assert!(is_rate_limited(&backoff));
        let unrelated = UsecaseError::Board("graphql errors: NOT_FOUND".into());
        assert!(!is_rate_limited(&unrelated));
    }

    #[test]
    fn is_rate_limited_matches_rest_transport_tag() {
        // Pv2-транспорт ходит REST (#1236): verify-фоллбэк обязан узнавать
        // тег «gh rest [rate_limited]» (SkippedRateLimited — ретраи не
        // продолжаются), иначе rate-limit рёбра читались бы как Mismatch.
        let rest_limited = UsecaseError::Board(
            "gh rest [RATE_LIMITED]: /users/x/projectsV2/5/items: core quota".into(),
        );
        assert!(is_rate_limited(&rest_limited), "REST-тег — тот же класс");
        let rest_permission = UsecaseError::Board(
            "gh rest [PERMISSION]: /users/x/projectsV2: HTTP 403 (не rate limit)".into(),
        );
        assert!(
            !is_rate_limited(&rest_permission),
            "REST Permission — не rate limit"
        );
    }

    #[test]
    fn is_transport_flap_matches_only_owned_tag() {
        // (#1330) транспортный тег — контракт детектора: чужие классы
        // (rate limit/permission) и легаси-сырые тексты классом не считаются
        let transport = gh_transport_tagged("не-JSON (HTTP 502 Bad Gateway): eof".into());
        assert!(is_transport_flap(&transport));
        let limited = UsecaseError::Board(
            "gh graphql [RATE_LIMITED]: GitHub rate limit (core): the quota is exhausted".into(),
        );
        assert!(
            !is_transport_flap(&limited),
            "rate limit — не транспортный класс"
        );
        let permission = UsecaseError::Board(
            "gh graphql [PERMISSION]: HTTP 403 (not rate limit): check GH_TOKEN / the token's permissions"
                .into(),
        );
        assert!(
            !is_transport_flap(&permission),
            "permission — не транспортный класс"
        );
        let legacy = UsecaseError::Board("graphql не-JSON (HTTP 502): eof".into());
        assert!(
            !is_transport_flap(&legacy),
            "сырой текст без тега — не транспортный класс"
        );
    }

    #[test]
    fn transport_retry_policy_retries_once_on_flap_only() {
        // (#1330) разовый транспортный флап → одна повторная попытка после
        // паузы; rate limit/permission/graphql errors — без ретрая (ADR-110)
        let flap = gh_transport_tagged("не-JSON (HTTP 502): eof".into());
        assert_eq!(
            retry_after_flap(&flap, 1),
            Some(std::time::Duration::from_secs(3)),
            "флап на первой попытке → пауза перед повтором"
        );
        assert_eq!(
            retry_after_flap(&flap, 2),
            None,
            "флап на второй попытке → исчерпание, Err наверх как есть"
        );
        let limited =
            UsecaseError::Board("gh graphql [RATE_LIMITED]: backoff for another 4 min".into());
        let permission = UsecaseError::Board(
            "gh graphql [PERMISSION]: HTTP 403 (not rate limit): check GH_TOKEN / the token's permissions"
                .into(),
        );
        let graphql_errors = UsecaseError::Board("graphql errors: NOT_FOUND".into());
        for error in [&limited, &permission, &graphql_errors] {
            assert_eq!(
                retry_after_flap(error, 1),
                None,
                "чужой класс — немедленный громкий Err без ретрая"
            );
        }
    }

    #[test]
    fn ttl_cache_invalidate_drops_key_only() {
        let cache: TtlCache<u64> = TtlCache::new(std::time::Duration::from_secs(60));
        let now = std::time::Instant::now();
        cache.put("a", 1, now);
        cache.put("b", 2, now);
        cache.invalidate("a");
        assert!(cache.get("a", now).is_none(), "сброшенный ключ — мимо");
        assert_eq!(cache.get("b", now).as_deref(), Some(&2), "сосед жив");
    }

    #[test]
    fn ttl_cache_invalidate_all_clears_every_key() {
        let cache: TtlCache<u64> = TtlCache::new(std::time::Duration::from_secs(60));
        let now = std::time::Instant::now();
        cache.put("a", 1, now);
        cache.put("b", 2, now);
        cache.invalidate_all();
        assert!(cache.get("a", now).is_none() && cache.get("b", now).is_none());
    }

    #[test]
    fn write_through_status_updates_cached_row() {
        let hub = GitHubHub::new("o/r".into(), "t".into());
        let now = std::time::Instant::now();
        let rows = vec![Task::new(7, "строка".into(), Status::Analys)];
        hub.snap_cache.put("B", (rows, BoardMeta::default()), now);
        hub.write_through_status("B", 7, Status::InProgress);
        let snap = hub
            .snap_cache
            .get("B", std::time::Instant::now())
            .expect("write-through сохраняет ключ");
        assert_eq!(
            snap.0.iter().find(|t| t.number() == 7).unwrap().status(),
            Status::InProgress,
            "собственная запись видна из кэша сразу (read-your-writes #1235)"
        );
    }

    /// Регресс-пин бага тест-раунда #1236 (19.11, живой прогон): Pv2 REST
    /// GET items/{id} (и list items) НЕ несёт single-select Status —
    /// проверено живьём: PATCH-ответ значение возвращает, GET — никогда,
    /// даже для строк, мутированных дни назад (асимметрия PATCH/GET).
    /// Контракт: REST-рида носителя (row_status) обязана видеть Status
    /// строки так же, как GraphQL-снапшот (эталон), — иначе гард edit()
    /// за окном write-through (60с) отвечает «строки нет на доске» на
    /// живую строку, и вердикт-переходы конвейера падают. Красный до
    /// фикса носителя-риды, зелёный после.
    #[test]
    #[ignore] // живой прогон: GH_TOKEN + сеть; read-only — снапшот + row_status
    fn live_row_status_reads_carrier_status_like_snapshot() {
        let repo =
            std::env::var("UWUWU_HUB_REPO").unwrap_or_else(|_| "yurvon-screamo/uwuwu-hub".into());
        let token = std::env::var("GH_TOKEN").expect("GH_TOKEN");
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async {
            let board = "Задачи uwuwu";
            let hub = GitHubHub::new(repo, token);
            let rows = hub
                .read_board(board)
                .await
                .expect("снапшот-эталон (GraphQL)");
            let (n, expected) = rows
                .iter()
                .map(|t| {
                    (
                        t.number(),
                        uwuwu_usecase::status_name(t.status()).to_string(),
                    )
                })
                .find(|(_, s)| !s.trim().is_empty())
                .expect("на живой доске есть строка с непустым статусом");
            let via_rest = hub
                .row_status(board, n)
                .await
                .expect("рида носителя не должна падать");
            assert_eq!(
                via_rest.map(uwuwu_usecase::status_name),
                Some(expected.as_str()),
                "REST-рида носителя слепа к Status: строка #{n} на доске «{board}» \
                 со статусом «{expected}» (GraphQL-эталон), row_status вернул {via_rest:?} \
                 — гард edit() отклонит вердикт-переход живой строки"
            );
        });
    }

    #[test]
    fn write_through_status_without_snapshot_is_noop() {
        let hub = GitHubHub::new("o/r".into(), "t".into());
        hub.write_through_status("B", 7, Status::InProgress);
        assert!(
            hub.snap_cache.get("B", std::time::Instant::now()).is_none(),
            "нет снапшота — write-through не создаёт пустой записи"
        );
    }

    #[test]
    fn write_through_status_row_absent_keeps_snapshot_intact() {
        // Живой случай тест-раунда #1235: GitHub-лаг — мутируемой строки
        // нет в снапшоте (проектный вью отдаёт частичное представление).
        // Write-through не паникует и не портит снапшот: недостающую
        // строку покажет refetch после TTL, чужие строки не тронуты.
        let hub = GitHubHub::new("o/r".into(), "t".into());
        let now = std::time::Instant::now();
        let rows = vec![Task::new(8, "другая строка".into(), Status::Todo)];
        hub.snap_cache.put("B", (rows, BoardMeta::default()), now);
        hub.write_through_status("B", 7, Status::InProgress);
        let snap = hub
            .snap_cache
            .get("B", std::time::Instant::now())
            .expect("ключ снапшота жив");
        assert_eq!(snap.0.len(), 1, "мусор в снапшот не добавлен");
        assert_eq!(
            snap.0.first().map(|t| t.number()).unwrap_or_default(),
            8,
            "чужие строки не тронуты"
        );
        assert_eq!(snap.0.first().map(|t| t.status()), Some(Status::Todo));
    }

    #[test]
    fn ttl_cache_hits_within_ttl_and_misses_after() {
        // #1243: кеш снапшотов/статусов — попадание внутри TTL, промах
        // после, перезапись по ключу
        let ttl = std::time::Duration::from_secs(120);
        let cache: TtlCache<u64> = TtlCache::new(ttl);
        let t0 = std::time::Instant::now();
        cache.put("board", 1, t0);
        assert_eq!(cache.get("board", t0 + ttl / 2).as_deref(), Some(&1));
        assert!(cache
            .get("board", t0 + ttl + std::time::Duration::from_secs(1))
            .is_none());
        cache.put("board", 2, t0 + ttl);
        assert_eq!(
            cache.get("board", t0 + ttl + ttl / 2).as_deref(),
            Some(&2),
            "перезапись продлевает окно"
        );
        assert!(cache.get("other", t0).is_none(), "чужой ключ — мимо");
    }

    #[test]
    fn gql_block_gates_only_until_deadline() {
        // #1243: RATE_LIMITED блокирует fail-fast до дедлайна, затем
        // разблокирует (окно rolling)
        let hub = GitHubHub::new("a/b".into(), "t".into());
        let t0 = std::time::Instant::now();
        assert!(hub.gql_blocked(t0).is_none(), "изначально не блокирован");
        hub.block_gql_until(t0 + std::time::Duration::from_secs(300));
        assert!(hub
            .gql_blocked(t0 + std::time::Duration::from_secs(299))
            .is_some());
        assert!(
            hub.gql_blocked(t0 + std::time::Duration::from_secs(301))
                .is_none(),
            "после дедлайна — разблокирован"
        );
    }

    /// Строковый контракт Direct на тик-пути снапшота (#1357, тестировщик):
    /// issue-backed шейп каталога /user/issue-fields — «on» поднимает флаг,
    /// «off» и чужая опция — нет; отсутствие поля — обычная строка.
    /// Мок-контур хранит булевы и этот парсинг минует — пин здесь.
    #[test]
    fn item_values_direct_flag_string_contract() {
        let item = |direct: &str| {
            json!({
                "content": { "__typename": "Issue", "number": 1357, "title": "t", "state": "open" },
                "fieldValues": { "nodes": [
                    { "__typename": "ProjectV2ItemIssueFieldValue",
                      "field": { "__typename": "ProjectV2SingleSelectField", "name": "Direct" },
                      "issueFieldValue": {
                          "__typename": "IssueFieldSingleSelectValue",
                          "name": direct, "value": direct, "optionId": "IFSSO_d"
                      } },
                ]}
            })
        };
        assert!(item_values(&item("on")).direct, "«on» — флаг поднят");
        assert!(!item_values(&item("off")).direct, "«off» — флаг опущен");
        assert!(
            !item_values(&item("true")).direct,
            "чужая опция флаг не поднимает (парсинг строгий)"
        );
        let bare = json!({
            "content": { "__typename": "Issue", "number": 1, "title": "t", "state": "open" },
            "fieldValues": { "nodes": [
                { "name": "In Progress", "field": { "name": "Status" } },
            ]}
        });
        assert!(!item_values(&bare).direct, "нет поля — обычная строка");
    }

    /// Pv2-own шейп (значение на верхнем уровне узла): Direct=on читается
    /// и оттуда — шейп колонок доски (#1357, тестировщик).
    #[test]
    fn item_values_direct_board_column_shape() {
        let item = json!({
            "content": { "__typename": "Issue", "number": 2, "title": "t", "state": "open" },
            "fieldValues": { "nodes": [
                { "name": "on", "field": { "name": "Direct" } },
            ]}
        });
        assert!(item_values(&item).direct);
    }

    #[test]
    fn item_values_parses_issue_backed_cells() {
        // #1174: поля каталога /user/issue-fields («Приоритет», «Autodone»)
        // приходят в Pv2 типом ProjectV2ItemIssueFieldValue с вложенным
        // issueFieldValue — шейп живой пробы доски «Задачи uwuwу»
        let item = json!({
            "content": { "__typename": "Issue", "number": 1174, "title": "t", "state": "open" },
            "fieldValues": { "nodes": [
                { "__typename": "ProjectV2ItemFieldRepositoryValue" },
                { "__typename": "ProjectV2ItemFieldLabelValue" },
                {
                    "__typename": "ProjectV2ItemIssueFieldValue",
                    "field": { "__typename": "ProjectV2SingleSelectField", "name": "Приоритет" },
                    "issueFieldValue": {
                        "__typename": "IssueFieldSingleSelectValue",
                        "name": "medium", "value": "medium", "optionId": "IFSSO_x"
                    }
                },
                {
                    "__typename": "ProjectV2ItemIssueFieldValue",
                    "field": { "__typename": "ProjectV2SingleSelectField", "name": "Autodone" },
                    "issueFieldValue": {
                        "__typename": "IssueFieldSingleSelectValue",
                        "name": "on", "value": "on", "optionId": "IFSSO_y"
                    }
                },
                {
                    "__typename": "ProjectV2ItemIssueFieldValue",
                    "field": { "__typename": "ProjectV2SingleSelectField", "name": "Automerge" },
                    "issueFieldValue": {
                        "__typename": "IssueFieldSingleSelectValue",
                        "name": "on", "value": "on", "optionId": "IFSSO_z"
                    }
                },
                { "__typename": "ProjectV2ItemFieldSingleSelectValue",
                  "name": "Design", "field": { "__typename": "ProjectV2FieldCommon", "name": "Status" } },
            ]}
        });
        let v = item_values(&item);
        assert_eq!(v.priority, "medium");
        assert!(v.automerge);
        assert_eq!(v.status, "Design");
    }

    #[test]
    #[ignore] // живой прогон: GH_TOKEN + сеть к api.github.com (read-only)
    fn live_snapshot_reads_issue_backed_automerge() {
        // #1174: живой e2e read-path — снапшот парсит issue-backed
        // single-select значения (каталог /user/issue-fields) из Pv2-отражения.
        // Носитель — sandbox #1265 (Autodone=on, раунд тестирования #1232,
        // строка открыта): отражение стабильно с 17.11.
        //
        // NB дрейф GitHub (дельта-раунд #1232, 18.11): Pv2-отражение
        // issue-backed значений стало избирательным. Приоритет перестал
        // отражаться вовсе (носители #1174/#1232/#1272 живы по REST,
        // ProjectV2ItemIssueFieldValue для «Приоритет» отсутствует); у
        // закрытой #1174 отражение существовало 17.11 и исчезло. Пин на
        // priority-через-снапшот на живом носителе не держится; приоритет
        // конвейерных стадий читается enrichment'ом (REST, #1163), для
        // не-stage строк (#1174-срез CUP-очереди) — слепота, флаг
        // владельцу в отчёте #1232.
        let repo =
            std::env::var("UWUWU_HUB_REPO").unwrap_or_else(|_| "yurvon-screamo/uwuwu-hub".into());
        let token = std::env::var("GH_TOKEN").expect("GH_TOKEN");
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let rows: Vec<Task> = rt
            .block_on(async {
                let hub = GitHubHub::new(repo, token);
                hub.read_board("Задачи sandbox").await
            })
            .expect("read_board");
        assert!(!rows.is_empty(), "доска не пуста");

        // носитель on (REST issue_field_values) — снапшот обязан показать то же
        let t = rows
            .iter()
            .find(|t| t.number() == 1265)
            .expect("#1265 на доске");
        assert!(
            t.automerge(),
            "снапшот слеп к issue-backed automerge-носителю (Autodone=on у #1265): \
             #1174-срез парсинга Pv2-отражения деградировал"
        );
    }

    #[test]
    #[ignore] // живой прогон: GH_TOKEN + сеть к api.github.com (read-only)
    fn live_read_issue_sees_native_issue_type() {
        // #1271: REST-путь read_issue обязан нести нативный issue Type —
        // issue["type"]["name"] → set_kind (fm-вью task_get). Строки
        // sandbox: #1265 — Feature на носителе, #1175 — native-null хвост.
        let repo =
            std::env::var("UWUWU_HUB_REPO").unwrap_or_else(|_| "yurvon-screamo/uwuwu-hub".into());
        let token = std::env::var("GH_TOKEN").expect("GH_TOKEN");
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async {
            let hub = GitHubHub::new(repo, token);
            let view = hub.read_issue("Задачи sandbox", 1265).await.expect("1265");
            assert_eq!(
                view.task.kind(),
                Some(uwuwu_domain::TaskType::Feature),
                "REST-вью слеп к нативному issue Type у #1265: \
                 read_issue-парс issue[\"type\"] деградировал (#1271)"
            );
            let view = hub.read_issue("Задачи sandbox", 1175).await.expect("1175");
            assert_eq!(
                view.task.kind(),
                None,
                "native-null хвост #1175 обязан читаться пустым типом без ошибки (#1271)"
            );
        });
    }

    #[test]
    #[ignore] // живой прогон: GH_TOKEN + сеть к api.github.com (read-only)
    fn live_read_issue_sees_automerge() {
        // БАГ-ПИН ручного прогона #1232 (tester, 17.11): fm-вью task_get
        // показывает automerge: false при Autodone=on на носителе —
        // read_issue собирает Task через map_task_fields, который не мапит
        // automerge из issue_field_values (тик-снапшот item_values видит
        // верно). Красный до фикса; зелёный — после. Строка sandbox #1265
        // («Автомерж-bounce: битый PR») выставлена в on ручным прогоном.
        let repo =
            std::env::var("UWUWU_HUB_REPO").unwrap_or_else(|_| "yurvon-screamo/uwuwu-hub".into());
        let token = std::env::var("GH_TOKEN").expect("GH_TOKEN");
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let view = rt
            .block_on(async {
                let hub = GitHubHub::new(repo, token);
                hub.read_issue("Задачи sandbox", 1265).await
            })
            .expect("read_issue");
        assert!(
            view.task.automerge(),
            "fm-вью read_issue слеп к automerge-носителю (Autodone=on у #1265): \
             баг-пин ручного прогона #1232"
        );
    }

    #[test]
    #[ignore] // живой прогон: GH_TOKEN + сеть к api.github.com (read-only)
    fn live_issue_fields_render_branchless_pr_cells() {
        // #1155: docs-only ячейки рендерятся против реального hub —
        // #52 (uwuwu, issue-URL) и #379 (true-washing, forgejo-pull):
        // значения в носителях живы (пробы #1157/#1251), fm обязан показывать
        let repo =
            std::env::var("UWUWU_HUB_REPO").unwrap_or_else(|_| "yurvon-screamo/uwuwu-hub".into());
        let token = std::env::var("GH_TOKEN").expect("GH_TOKEN");
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let (uwuwu52, tw379): (uwuwu_domain::TaskFields, uwuwu_domain::TaskFields) =
            rt.block_on(async {
                let hub = GitHubHub::new(repo, token);
                let a = hub.issue_fields(52).await.expect("#52 issue_fields");
                let b = hub.issue_fields(379).await.expect("#379 issue_fields");
                (a, b)
            });
        let uwuwu52_url = uwuwu52.pr().map(|u| u.as_str()).unwrap_or_default();
        assert!(
            uwuwu52_url.contains("github.com/yurvon-screamo/uwuwu-hub/issues/52"),
            "#52: issue-URL в fm, got {uwuwu52_url:?}"
        );
        assert!(uwuwu52.branch().is_none(), "#52: ветки кода нет");
        let tw379_url = tw379.pr().map(|u| u.as_str()).unwrap_or_default();
        assert!(
            tw379_url.contains("git.true-wash.com/true-wash/deploy/pulls/9"),
            "#379: forgejo-pull в fm, got {tw379_url:?}"
        );
        assert!(tw379.branch().is_none(), "#379: ветки кода нет");
    }

    #[test]
    fn item_values_maps_board_columns() {
        let item = json!({
            "content": { "__typename": "Issue", "number": 1, "title": "t", "state": "open" },
            "fieldValues": { "nodes": [
                { "name": "In Progress", "field": { "name": "Status" } },
                { "name": "high", "field": { "name": "Приоритет" } },
                { "name": "on", "field": { "name": "Autodone" } },
                { "name": "on", "field": { "name": "Automerge" } },
            ]}
        });
        let v = item_values(&item);
        assert_eq!(v.status, "In Progress");
        assert_eq!(v.priority, "high");
        assert!(v.automerge);
    }

    #[test]
    fn parse_page_marks_closed_done() {
        let items = vec![json!({
            "id": "IT_x",
            "content": { "__typename": "Issue", "number": 7, "title": "t", "state": "closed" },
            "fieldValues": { "nodes": [] }
        })];
        let mut meta = BoardMeta::default();
        let rows = parse_page(&items, &mut meta);
        assert_eq!(rows[0].status(), Status::Done);
        assert_eq!(meta.items.get(&7).map(String::as_str), Some("IT_x"));
    }

    #[test]
    fn guard_status_resolves_closed_issue_over_stale_field() {
        // #1403: расщеплённая строка — закрытый мимо task_close issue
        // терминален при любом парсе поля, включая мусорное (Empty)
        assert_eq!(guard_status(Status::ReviewResult, Some(true)), Status::Done);
        assert_eq!(guard_status(Status::Empty, Some(true)), Status::Done);
    }

    #[test]
    fn guard_status_keeps_field_parse_for_open_issue() {
        // открытый issue — гард отвечает парсом поля, как прежде
        assert_eq!(
            guard_status(Status::ReviewResult, Some(false)),
            Status::ReviewResult
        );
    }

    #[test]
    fn guard_status_degrades_to_field_parse_when_state_unreadable() {
        // деградационный пин (#1403): отказ чтения state — прежнее
        // поведение (парс поля), не тихий Done и не Err
        assert_eq!(
            guard_status(Status::ReviewResult, None),
            Status::ReviewResult
        );
    }

    #[test]
    fn parse_page_carries_native_issue_type() {
        // #1271: issueType с issue-узла снапшота доезжает до агрегата;
        // null (native-null хвост) и нативный дефолт Task — None без ошибки
        let mut meta = BoardMeta::default();
        let items = vec![
            json!({
                "id": "IT_k1",
                "content": { "__typename": "Issue", "number": 1, "title": "a",
                             "state": "open", "issueType": { "name": "Bug" } },
                "fieldValues": { "nodes": [] }
            }),
            json!({
                "id": "IT_k2",
                "content": { "__typename": "Issue", "number": 2, "title": "b",
                             "state": "open", "issueType": { "name": "Refactoring" } },
                "fieldValues": { "nodes": [] }
            }),
            json!({
                "id": "IT_k3",
                "content": { "__typename": "Issue", "number": 3, "title": "c",
                             "state": "open", "issueType": null },
                "fieldValues": { "nodes": [] }
            }),
            json!({
                "id": "IT_k4",
                "content": { "__typename": "Issue", "number": 4, "title": "d",
                             "state": "open", "issueType": { "name": "Task" } },
                "fieldValues": { "nodes": [] }
            }),
        ];
        let rows = parse_page(&items, &mut meta);
        assert_eq!(rows[0].kind(), Some(TaskType::Bug));
        assert_eq!(rows[1].kind(), Some(TaskType::Refactoring));
        assert_eq!(rows[2].kind(), None, "native-null хвост — пустой тип");
        assert_eq!(rows[3].kind(), None, "нативный дефолт Task — пустой тип");
    }

    #[test]
    fn parse_page_carries_closed_at() {
        // #1293: closedAt с носителя доезжает до агрегата — вход клина
        let mut meta = BoardMeta::default();
        let items = vec![json!({
            "id": "IT_c",
            "content": {
                "__typename": "Issue", "number": 9, "title": "t", "state": "closed",
                "closedAt": "2026-11-10T00:00:00Z"
            },
            "fieldValues": { "nodes": [] }
        })];
        let rows = parse_page(&items, &mut meta);
        assert_eq!(rows[0].closed_at(), Some(1_794_268_800_000));
        // открытый issue — даты нет
        let mut meta2 = BoardMeta::default();
        let open = vec![json!({
            "id": "IT_o",
            "content": { "__typename": "Issue", "number": 10, "title": "t", "state": "open", "closedAt": null },
            "fieldValues": { "nodes": [] }
        })];
        let rows2 = parse_page(&open, &mut meta2);
        assert_eq!(rows2[0].closed_at(), None);
    }
}

#[cfg(test)]
mod slot_mapping_tests {
    use super::*;

    #[test]
    fn dev_session_field_fills_design_slot() {
        // инцидент 15/16.11: Design-строка с заполненной ячейкой «Сессия dev»
        // читалась без сессии → тик спамил спавн-дублей (бюджет маскировал)
        let mut raw = Fields::new();
        raw.insert("Сессия dev".into(), "ses_abc123".into());
        let f = map_task_fields(&raw);
        let t = uwuwu_domain::Task::new(1, "t".into(), uwuwu_domain::Status::Design);
        let mut task = t;
        task.apply_fields(f);
        assert_eq!(
            task.session_of(uwuwu_domain::Slot::Design),
            Some("ses_abc123")
        );
        assert_eq!(task.session_of(uwuwu_domain::Slot::Dev), Some("ses_abc123"));
        assert!(
            task.stage_needs_spawn().is_none(),
            "Design-слот видит сессию"
        );
    }

    #[test]
    fn review_session_field_fills_test_review_slot() {
        // #1413: fm-ячейка «Сессия ревью» — общая для трёх ревью-слотов
        // (вектора assignments — не-экзаустивная поверхность вне
        // компилятора): строка в «Ревью тестирования» с живой ячейкой
        // видит сессию — тик не спавнит дубли (класс инцидента 15/16.11)
        let mut raw = Fields::new();
        raw.insert("Сессия ревью".into(), "ses_testrev".into());
        let f = map_task_fields(&raw);
        let mut task = uwuwu_domain::Task::new(2, "t".into(), uwuwu_domain::Status::ReviewTesting);
        task.apply_fields(f);
        assert_eq!(
            task.session_of(uwuwu_domain::Slot::TestReview),
            Some("ses_testrev")
        );
        assert_eq!(
            task.session_of(uwuwu_domain::Slot::ResultReview),
            Some("ses_testrev")
        );
        assert_eq!(
            task.session_of(uwuwu_domain::Slot::PlanReview),
            Some("ses_testrev")
        );
        assert_eq!(
            task.session_of(uwuwu_domain::Slot::Dev),
            None,
            "ревью-ячейка не течёт в дев-слот"
        );
        assert_eq!(
            task.session_of(uwuwu_domain::Slot::Test),
            None,
            "ревью-ячейка не течёт в тест-слот"
        );
        assert!(
            task.stage_needs_spawn().is_none(),
            "TestReview-слот видит сессию — спавн-дубля нет"
        );
    }

    #[test]
    fn priority_field_absent_maps_empty() {
        // отсутствующее поле — самый частый кейс доски: не High и не паника
        let f = map_task_fields(&Fields::new());
        assert_eq!(f.priority(), uwuwu_domain::Priority::Empty);
    }

    #[test]
    fn priority_field_maps_russian_variants() {
        // enrichment-носитель несёт и русские опции — единый парсер
        // (naming::parse_priority) обязан их мапить
        for (value, expected) in [
            ("высокий", uwuwu_domain::Priority::High),
            ("Высокий", uwuwu_domain::Priority::High),
            ("средний", uwuwu_domain::Priority::Medium),
            ("низкий", uwuwu_domain::Priority::Low),
        ] {
            let mut raw = Fields::new();
            raw.insert(crate::github::fields::F_PRIORITY.into(), value.into());
            assert_eq!(map_task_fields(&raw).priority(), expected, "value: {value}");
        }
    }

    #[test]
    fn priority_field_maps_case_insensitive() {
        // #1170: опции доски в нижнем регистре — high обязан доезжать High,
        // иначе гейт пика морозил ВСЕ спавны, включая high
        let mut raw = Fields::new();
        raw.insert("Приоритет".into(), "high".into());
        let f = map_task_fields(&raw);
        assert_eq!(f.priority(), uwuwu_domain::Priority::High);

        let mut raw = Fields::new();
        raw.insert("Приоритет".into(), "Medium".into());
        assert_eq!(
            map_task_fields(&raw).priority(),
            uwuwu_domain::Priority::Medium
        );

        let mut raw = Fields::new();
        raw.insert("Приоритет".into(), "что-то-иное".into());
        assert_eq!(
            map_task_fields(&raw).priority(),
            uwuwu_domain::Priority::Empty
        );
    }
}

#[cfg(test)]
mod vcs_mapping_tests {
    use super::*;

    #[test]
    fn branchless_artifact_url_renders_pr() {
        // #1155: вики-артефакт без ветки (канон ADR-021, жертва #1153) —
        // рендер pr без Branch, а не отброс
        let mut raw = Fields::new();
        raw.insert(
            "PR".into(),
            "https://github.com/yurvon-screamo/uwuwu-hub/blob/main/docs/Projects/uwuwu/Design%20Spec.md"
                .into(),
        );
        let f = map_task_fields(&raw);
        assert!(
            f.pr().is_some(),
            "безветочный артефакт-URL обязан рендериться в fm"
        );
        assert!(f.branch().is_none());
    }

    #[test]
    fn branchless_foreign_pull_url_renders_pr() {
        // #1155/true-washing#379: forgejo-pull на безветочной строке —
        // значение живо в носителе, рендер обязан его показывать
        let mut raw = Fields::new();
        raw.insert(
            "PR".into(),
            "https://git.true-wash.com/true-wash/deploy/pulls/9".into(),
        );
        let f = map_task_fields(&raw);
        assert!(f.pr().is_some(), "чужовендорный pull без ветки — рендер");
        assert!(f.branch().is_none());
    }

    #[test]
    fn branch_and_pr_pair_maps_pull_request() {
        let mut raw = Fields::new();
        raw.insert("Branch".into(), "work/task-1".into());
        raw.insert(
            "PR".into(),
            "https://github.com/yurvon-screamo/uwuwu-agent/pull/293".into(),
        );
        let f = map_task_fields(&raw);
        assert!(f.pr().is_some());
        assert_eq!(f.branch().map(|b| b.as_str()), Some("work/task-1"));
    }

    #[test]
    fn branch_with_artifact_url_pair_maps_pull_request() {
        // #1153 после воркараунда: Branch=main + blob-URL — кодовая пара
        // с артефакт-URL живёт в PullRequest до очистки ветки
        let mut raw = Fields::new();
        raw.insert("Branch".into(), "main".into());
        raw.insert(
            "PR".into(),
            "https://github.com/yurvon-screamo/uwuwu-hub/blob/main/docs/x.md".into(),
        );
        let f = map_task_fields(&raw);
        assert!(f.pr().is_some());
        assert_eq!(f.branch().map(|b| b.as_str()), Some("main"));
    }

    #[test]
    fn branch_without_pr_maps_branch() {
        let mut raw = Fields::new();
        raw.insert("Branch".into(), "work/task-2".into());
        let f = map_task_fields(&raw);
        assert!(f.pr().is_none());
        assert_eq!(f.branch().map(|b| b.as_str()), Some("work/task-2"));
    }

    #[test]
    fn empty_vcs_fields_map_local() {
        let f = map_task_fields(&Fields::new());
        assert!(f.pr().is_none());
        assert!(f.branch().is_none());
    }
}
