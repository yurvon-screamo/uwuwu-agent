//! gh-hub adapter: доска (bulk select-колонки) + issue REST (тело/комменты/
//! нативные поля). fm-зеркало снесено (владелец 15.11): операционные данные —
//! Status/Приоритет/Automerge на доске, сессии/ветка/PR — глобальные поля issue
//! (fields.rs). Тело задачи — чистый контент.

use super::fields::{parse_field_values, Fields, IssueFields};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use uwuwu_domain::{Status, Task};
use uwuwu_usecase::{CommentRef, IssueView, TaskBoardRepo};
use uwuwu_usecase::{UsecaseError, UsecaseResult};

pub struct GitHubHub {
    repo: String,
    token: String,
    http: reqwest::Client,
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
              content{ __typename ...on Issue{ number title state } }
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
struct TtlCache<T> {
    ttl: std::time::Duration,
    cells: std::sync::RwLock<std::collections::HashMap<String, (std::time::Instant, Arc<T>)>>,
}

impl<T> TtlCache<T> {
    fn new(ttl: std::time::Duration) -> Self {
        Self {
            ttl,
            cells: std::sync::RwLock::new(std::collections::HashMap::new()),
        }
    }

    fn get(&self, key: &str, now: std::time::Instant) -> Option<Arc<T>> {
        let guard = self.cells.read().expect("ttl cache lock");
        match guard.get(key) {
            Some((at, v)) if now.duration_since(*at) < self.ttl => Some(v.clone()),
            _ => None,
        }
    }

    fn put(&self, key: &str, value: T, now: std::time::Instant) -> Arc<T> {
        let arc = Arc::new(value);
        self.cells
            .write()
            .expect("ttl cache lock")
            .insert(key.to_string(), (now, arc.clone()));
        arc
    }

    /// Точечный сброс ключа: мутация доски — read-your-writes (#1235).
    fn invalidate(&self, key: &str) {
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

/// Rate-limit отказ транспорта: только собственный тег gql-точки,
/// не строковые эвристики по текстам GitHub.
fn is_rate_limited(e: &UsecaseError) -> bool {
    e.to_string()
        .to_lowercase()
        .contains("gh graphql [rate_limited]")
}

#[derive(Default, Clone)]
struct BoardMeta {
    project_id: String,
    status_field_id: String,
    options: std::collections::HashMap<String, String>,
    items: std::collections::HashMap<u64, String>,
}

impl GitHubHub {
    pub fn new(repo: String, token: String) -> Self {
        Self {
            fields: Arc::new(IssueFields::new(repo.clone(), token.clone())),
            repo,
            token,
            http: reqwest::Client::builder()
                .user_agent(concat!("uwuwu-server/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("reqwest client"),
            // 120 c ≤ тик (poll 180 c): снимок освежается раз в тик,
            // мутации тика шарят один снимок
            snap_cache: TtlCache::new(std::time::Duration::from_secs(120)),
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
                "gh graphql [RATE_LIMITED]: бэкофф ещё {:?}",
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
            .map_err(|e| UsecaseError::Board(format!("graphql transport: {e}")))?;
        let status = resp.status();
        let headers = resp.headers().clone();
        let text = resp
            .text()
            .await
            .map_err(|e| UsecaseError::Board(format!("graphql body: {e}")))?;
        if let Some(fail) = crate::gherr::classify(status, &headers) {
            return Err(gh_tagged(&fail));
        }
        let body: Value = serde_json::from_str(&text)
            .map_err(|e| UsecaseError::Board(format!("graphql не-JSON (HTTP {status}): {e}")))?;
        if !status.is_success() {
            return Err(UsecaseError::Board(format!(
                "gh api {status}: {}",
                trunc(&text)
            )));
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
            .ok_or_else(|| UsecaseError::Board("graphql: нет data".into()))
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
            UsecaseError::Board(format!("repo в форме owner/name: {}", self.repo))
        })?;
        let mut meta = BoardMeta::default();
        let mut rows: Vec<Task> = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let data = self
                .gql(
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
                .ok_or_else(|| UsecaseError::Board("projectsV2.nodes не массив".into()))?;
            let project = nodes
                .iter()
                .find(|p| p["title"].as_str() == Some(board))
                .ok_or_else(|| {
                    UsecaseError::Board(format!(
                        "доска «{board}» не найдена среди {} шт",
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
                        "доска «{board}»: нет project id"
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
            "доска «{board}» превысила {MAX_PAGES} страниц — гард"
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
            .gql(
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

    /// Статусный value строки КОНКРЕТНОЙ доски (project-фильтр по тайтлу):
    /// None — строки нет на этой доске; Some("") — есть, но Status не задан.
    /// Против board_status_fetch, который берёт Status первой попавшейся
    /// привязки (#1235 Low-1).
    async fn carrier_status(&self, board: &str, number: u64) -> UsecaseResult<Option<String>> {
        let q = r#"
          query($owner:String!,$name:String!,$n:Int!){
            repository(owner:$owner,name:$name){
              issue(number:$n){
                projectItems(first:10){
                  nodes{
                    project{ title }
                    fieldValues(first:20){ nodes{
                      __typename ...on ProjectV2ItemFieldSingleSelectValue{
                        name field{ ...on ProjectV2FieldCommon{ name } } } } }
                  }
                }
              }
            }
          }"#;
        let (owner, name) = self
            .repo
            .split_once('/')
            .ok_or_else(|| UsecaseError::Board(format!("repo: {}", self.repo)))?;
        let data = self
            .gql(
                q,
                json!({ "owner": owner, "name": name, "n": number as i64 }),
            )
            .await?;
        let Some(items) = data["repository"]["issue"]["projectItems"]["nodes"].as_array() else {
            return Ok(None);
        };
        for item in items {
            if item["project"]["title"].as_str() != Some(board) {
                continue;
            }
            let Some(vals) = item["fieldValues"]["nodes"].as_array() else {
                return Ok(Some(String::new()));
            };
            for v in vals {
                if v["field"]["name"].as_str() == Some("Status") {
                    return Ok(Some(v["name"].as_str().unwrap_or_default().to_string()));
                }
            }
            return Ok(Some(String::new()));
        }
        Ok(None)
    }

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

    /// Контрольное чтение после мутации статуса (семья #1213): до 3
    /// попыток с паузой 1 с. Rate limit — ретраи не продолжаются
    /// (мутация принята GitHub); сетевой сбой и рассогласование —
    /// полный цикл, дальше громкий отказ.
    async fn verify_status(&self, number: u64, expected: Status) -> VerifyOutcome {
        for attempt in 0..3 {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            match self.board_status_fetch(number).await {
                Ok(fetched) if uwuwu_usecase::parse_status(&fetched) == expected => {
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
    /// не держим возможно-ложное значение 60–120 с (семья #1213).
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
                return Err(UsecaseError::Board(format!(
                    "статус #{number}: запись «{}» не подтвердилась чтением — \
                     GitHub отдаёт лаг или мутация не применилась; не ретраить слепо",
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
            task.set_priority(uwuwu_usecase::parse_priority(&v.priority));
            task.set_automerge(v.automerge);
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
}

fn item_values(item: &Value) -> ItemValues {
    let mut out = ItemValues {
        status: String::new(),
        priority: String::new(),
        automerge: false,
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
                crate::github::fields::F_PRIORITY | "Priority" => out.priority = value.to_string(),
                // автомерж (#1232): новое имя, legacy «Autodone» — back-compat
                "Automerge" | "Autodone" => out.automerge = value == "on" || value == "true",
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
    // dev-поле — оба слота дев-семьи (Design/Dev единая сессия, канон
    // сессий #328): иначе Design-строка читается без сессии и тик
    // бесконечно пытается спавнить дубль (инцидент 15.11/16.11 —
    // budget-отказы маскировали, budget-50 вскрыл бы спавном дублей)
    let mut assignments = std::collections::BTreeMap::new();
    for (name, slots) in [
        (
            crate::github::fields::F_DEV_SESSION,
            vec![Slot::Dev, Slot::Design],
        ),
        (
            crate::github::fields::F_REVIEW_SESSION,
            vec![Slot::PlanReview, Slot::ResultReview],
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
    // Без мапинга гейт пика морозил бы ВСЕ спавны, включая high
    // (инцидент #1170: high-таска deferred_peak'илась, ревьювер не
    // поднимался в окно GLM).
    let priority = raw
        .get(crate::github::fields::F_PRIORITY)
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
        let snap = self.snapshot(board).await?;
        // last-known статус строки на момент мутации — для ребра в логах
        // (Low-2 код-ревью #1235; значение со снапшота, может быть стэйл)
        let from = snap
            .0
            .iter()
            .find(|t| t.number() == number)
            .map(|t| uwuwu_usecase::status_name(t.status()).to_string());
        let option = snap
            .1
            .options
            .get(uwuwu_usecase::status_name(status))
            .cloned()
            .ok_or_else(|| {
                UsecaseError::Board(format!(
                    "Status-опция «{}» не найдена",
                    uwuwu_usecase::status_name(status)
                ))
            })?;
        let item = snap.1.items.get(&number).cloned().ok_or_else(|| {
            UsecaseError::Board(format!("строка #{number} не на доске «{board}»"))
        })?;
        let mutation = r#"
          mutation($p:ID!,$i:ID!,$f:ID!,$o:String!){
            updateProjectV2ItemFieldValue(input:{
              projectId:$p, itemId:$i, fieldId:$f, value:{singleSelectOptionId:$o}
            }){ projectV2Item{ id } }
          }"#;
        let (project_id, status_field_id) =
            (snap.1.project_id.clone(), snap.1.status_field_id.clone());
        self.gql(
            mutation,
            json!({ "p": project_id, "i": item, "f": status_field_id, "o": option }),
        )
        .await?;
        // read-your-writes (#1235): собственная запись обслуживается кэшем,
        // а не refetch'ем — read-лаг Projects v2 минутного масштаба не виден.
        self.write_through_status(board, number, status);
        self.write_through_status_cache(number, status);
        // verify-after-write (семья #1213): мутация либо подтверждена
        // чтением, либо громкая ошибка с откатом write-through.
        let outcome = self.verify_status(number, status).await;
        self.settle_write_through(board, number, outcome, status)?;
        tracing::info!(
            event = "board.set_status",
            board,
            number,
            from = from.as_deref().unwrap_or("—"),
            to = %uwuwu_usecase::status_name(status),
            "Статус строки обновлён и подтверждён чтением (from — last-known со снапшота)"
        );
        Ok(())
    }

    async fn row_status(&self, board: &str, number: u64) -> UsecaseResult<Option<Status>> {
        // project-фильтр: номер на чужой доске — не наша строка (Low-1
        // код-ревью #1235), а не «статус этой доски»
        match self.carrier_status(board, number).await? {
            // Status-значения на носителе нет (строка не на доске /
            // поле не задано); непустое непарсящееся — Some(Empty)
            None => Ok(None),
            Some(raw) if raw.trim().is_empty() => Ok(None),
            Some(raw) => Ok(Some(uwuwu_usecase::parse_status(&raw))),
        }
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
            uwuwu_domain::Slot::PlanReview | uwuwu_domain::Slot::ResultReview => {
                crate::github::fields::F_REVIEW_SESSION
            }
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
            uwuwu_domain::Slot::PlanReview | uwuwu_domain::Slot::ResultReview => {
                crate::github::fields::F_REVIEW_SESSION
            }
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
    ) -> UsecaseResult<u64> {
        let (status, resp) = self
            .rest(
                reqwest::Method::POST,
                &format!("/repos/{}/issues", self.repo),
                Some(
                    json!({ "title": title, "body": body, "labels": [format!("proj:{project}")] }),
                ),
            )
            .await?;
        if !status.is_success() {
            return Err(UsecaseError::Board(format!(
                "create issue: HTTP {status}: {}",
                resp.to_string().chars().take(140).collect::<String>()
            )));
        }
        let number = resp["number"].as_u64().unwrap_or_default();
        if number == 0 {
            return Err(UsecaseError::Board("create issue: нет номера".into()));
        }
        let node_id = resp["node_id"].as_str().unwrap_or_default().to_string();
        self.add_to_board(board, number, &node_id).await?;
        Ok(number)
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
                "replace body #{number}: не применилось (HTTP {status})"
            )));
        }
        Ok(())
    }

    async fn latest_comments(&self, number: u64) -> UsecaseResult<Vec<CommentRef>> {
        self.all_comments(number).await
    }
}

impl GitHubHub {
    /// Поля задачи (нативные): лениво для строк-кандидатов тика.
    #[allow(dead_code)] // точка входа ленивых чтений тика (срез анти-шторма подключит)
    pub async fn load_fields(&self, number: u64) -> UsecaseResult<Fields> {
        self.fields.load(number).await
    }

    async fn add_to_board(&self, board: &str, number: u64, node_id: &str) -> UsecaseResult<()> {
        let snap = self.snapshot(board).await?;
        let (project_id, status_field_id, analys) = (
            snap.1.project_id.clone(),
            snap.1.status_field_id.clone(),
            snap.1.options.get("Analys").cloned(),
        );
        let mutation = r#"
          mutation($p:ID!,$c:ID!){ addProjectV2ItemById(input:{projectId:$p, contentId:$c}){
            item{ id } } }"#;
        let added = self
            .gql(mutation, json!({ "p": project_id, "c": node_id }))
            .await?;
        let item = added["addProjectV2ItemById"]["item"]["id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if item.is_empty() {
            return Err(UsecaseError::Board(format!("add #{number}: нет item id")));
        }
        // create-дефолт Analys (#1135): опция ставится по имени колонки;
        // колонки нет — строка остаётся без статуса (rollout плана)
        if let Some(analys) = analys {
            let set = r#"
              mutation($p:ID!,$i:ID!,$f:ID!,$o:String!){
                updateProjectV2ItemFieldValue(input:{projectId:$p,itemId:$i,fieldId:$f,
                  value:{singleSelectOptionId:$o}}){ projectV2Item{ id } } }"#;
            self.gql(
                set,
                json!({ "p": project_id, "i": item, "f": status_field_id, "o": analys }),
            )
            .await?;
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
mod tests {
    use super::*;
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
            err.to_string().contains("не подтвердилась"),
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

    #[test]
    fn is_rate_limited_matches_only_owned_tag_not_github_texts() {
        // Permission-отказ: Display содержит «не rate limit» — старый
        // строковый матч читал его как rate limit (код-ревью Common-1)
        let permission = UsecaseError::Board(
            "gh graphql [PERMISSION]: HTTP 403 (не rate limit): проверь GH_TOKEN/права токена"
                .into(),
        );
        assert!(!is_rate_limited(&permission), "Permission — не rate limit");
        let limited = UsecaseError::Board(
            "gh graphql [RATE_LIMITED]: GitHub rate limit (core): квота исчерпана".into(),
        );
        assert!(is_rate_limited(&limited));
        let backoff = UsecaseError::Board("gh graphql [RATE_LIMITED]: бэкофф ещё 4 мин".into());
        assert!(is_rate_limited(&backoff));
        let unrelated = UsecaseError::Board("graphql errors: NOT_FOUND".into());
        assert!(!is_rate_limited(&unrelated));
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
        assert_eq!(snap.0[0].number(), 8, "чужие строки не тронуты");
        assert_eq!(snap.0[0].status(), Status::Todo);
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
        let mut meta = BoardMeta::default();
        let items = vec![json!({
            "id": "IT_x",
            "content": { "__typename": "Issue", "number": 7, "title": "t", "state": "closed" },
            "fieldValues": { "nodes": [] }
        })];
        let rows = parse_page(&items, &mut meta);
        assert_eq!(rows[0].status(), Status::Done);
        assert_eq!(meta.items.get(&7).map(String::as_str), Some("IT_x"));
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
