//! opencode-исполнитель: REST-клиент, реализация AgentSessionGateway.
//! Endpoints сверен с openapi опенкода 2.3.0.

use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::Duration;
use uwuwu_domain::ModelRef;
use uwuwu_usecase::{AgentSessionGateway, AssistantReply, Liveness, SessionInfo, SessionSpec};
use uwuwu_usecase::{UsecaseError, UsecaseResult};

/// Пауза опроса ленты сессии (канал вердиктов shell env-gc).
const USER_MSG_POLL: Duration = Duration::from_millis(3_000);

pub struct OpencodeClient {
    base: String,
    password: String,
    http: reqwest::Client,
    /// Кэш лимитов окна контекста из каталога моделей (#1221, ADR-082):
    /// ключ `provider/id`. None кэшируется НАВСЕГДА — перманентное
    /// отсутствие модели/лимита в каталоге; transient-сбой каталога
    /// (Err) не кэшируется — ретрай следующим тиком (фиксация ревью
    /// круга 1 ADR-082).
    model_limits: std::sync::Mutex<std::collections::HashMap<String, Option<u64>>>,
}

/// Активность сообщения (ms): opencode 2.3 не несёт `time.updated` в
/// message-ответе — живые поля created/firstToken/streamed/completed
/// (живая проба прода 17.11: хвост = {created} для idle, {created,
/// firstToken, streamed, completed} для assistant). Порядок: самое
/// позднее из доступных.
fn message_activity_ms(msg: &Value) -> Option<u64> {
    let t = &msg["time"];
    t["updated"]
        .as_u64()
        .or_else(|| t["completed"].as_u64())
        .or_else(|| t["streamed"].as_u64())
        .or_else(|| t["firstToken"].as_u64())
        .or_else(|| t["created"].as_u64())
}

/// Session busy verdict (#1231 semantics, #409 zombie guard, #1547 liveness):
/// busy iff busy > idle and the session keeps updating. The age of `time.busy`
/// alone does not prove an aborted turn: a long live turn still writes messages,
/// while a crashed turn keeps busy > idle forever but stops updating.
fn session_busy(s: &serde_json::Value, now_ms: u64) -> bool {
    const ZOMBIE_BUSY_MS: u64 = 30 * 60_000;
    let busy_ms = s["time"]["busy"].as_u64();
    let idle_ms = s["time"]["idle"].as_u64();
    let updated_ms = s["time"]["updated"].as_u64();
    busy_ms.is_some_and(|b| idle_ms.is_none_or(|i| b > i))
        && updated_ms.is_some_and(|u| now_ms.saturating_sub(u) < ZOMBIE_BUSY_MS)
}

/// Тело ответа message-эндпоинта opencode: голый массив ИЛИ обёртка
/// {data: [...]} (обе формы в живом проде, opencode 2.3 — инцидент
/// «строки стоят, сессии спят» 17.11: last_activity_ms не знал обёртки,
/// вечное Ok(None) выключало сталл-детект целиком).
fn message_array(resp: &Value) -> Option<&Vec<Value>> {
    resp.as_array().or_else(|| resp["data"].as_array())
}

/// Текст shell-сообщения живой формы (opencode 2.3.2, живая проба #1526):
/// {"type":"shell","command","status","output":{"output",...}}.
/// None — сообщение ещё в полёте (status running) или чужое.
fn shell_output(msg: &Value, command: &str) -> Option<String> {
    if msg["type"].as_str()? != "shell" {
        return None;
    }
    if msg["command"].as_str()? != command {
        return None;
    }
    match msg["status"].as_str()? {
        "running" => None,
        _ => Some(
            msg["output"]["output"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        ),
    }
}

/// Хвост журнала (order=desc): является ли самый свежий содержательный
/// блок (user|assistant) ассистентским. Служебные записи поверх ответа
/// (idle и пр., #1150/#1206 Common-1) пропускаются.
/// Страница списка сессий: сессии + курсор следующей страницы.
/// Курсор валиден только после ПОЛНОЙ страницы — opencode ставит
/// `cursor.next` даже на исчерпанном списке (проба живого API 17.11),
/// терминация пагинации — по неполной странице.
fn parse_session_page(
    resp: &serde_json::Value,
    limit: usize,
) -> (Vec<SessionInfo>, Option<String>) {
    let raw = if resp.is_array() {
        resp.clone()
    } else {
        resp["data"].clone()
    };
    let sessions: Vec<SessionInfo> = raw
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|s| {
                    // busy ⟺ busy > idle (сверено с бинарём 2.3.0, #1231),
                    // зомби-гард 30 мин (PR #409) — см. session_busy.
                    // При апгрейде opencode семантику перепроверить.
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or_default();
                    let busy = session_busy(s, now_ms);
                    Some(SessionInfo {
                        id: s["id"].as_str()?.to_string(),
                        title: s["title"].as_str().unwrap_or_default().to_string(),
                        agent: s["agent"].as_str().unwrap_or_default().to_string(),
                        directory: s["location"]["directory"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        updated_ms: s["time"]["updated"].as_u64().unwrap_or_default(),
                        outcome: s["outcome"].as_str().unwrap_or_default().to_string(),
                        total_tokens: s["tokens"]["input"].as_u64().unwrap_or_default()
                            + s["tokens"]["output"].as_u64().unwrap_or_default(),
                        busy,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let next = if sessions.len() >= limit {
        resp["cursor"]["next"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(String::from)
    } else {
        None
    };
    (sessions, next)
}

fn latest_meaningful_is_assistant(msgs: &[serde_json::Value]) -> bool {
    msgs.iter()
        .find(|m| matches!(m["type"].as_str(), Some("user") | Some("assistant")))
        .map(|m| m["type"] == "assistant")
        .unwrap_or(false)
}

/// Токены assistant-поворота (Session.Message.Assistant.tokens): сумма
/// полей = занятость окна контекста, та же арифметика, что у TUI
/// (ADR-082, живые пробы 2.2.6/2.3). input обязателен — user/compaction/
/// служебные записи токенов не несут и отбрасываются формой.
fn turn_tokens(msg: &serde_json::Value) -> Option<u64> {
    let t = &msg["tokens"];
    let input = t["input"].as_u64()?;
    let sum = |field: &str| t[field].as_u64().unwrap_or(0);
    Some(
        input
            + sum("output")
            + sum("reasoning")
            + t["cache"]["read"].as_u64().unwrap_or(0)
            + t["cache"]["write"].as_u64().unwrap_or(0),
    )
}

/// Токены свежайшего assistant-поворота desc-хвоста; None — содержательных
/// поворотов нет (пустая/свежая сессия — компактировать нечего).
fn newest_assistant_tokens(msgs: &[serde_json::Value]) -> Option<u64> {
    msgs.iter()
        .filter(|m| m["type"] == "assistant")
        .find_map(turn_tokens)
}

/// HTTP-статус → liveness-вердикт (маппинг вынесен для тестов без сети).
pub fn liveness_of_status(status: reqwest::StatusCode) -> Liveness {
    if status.is_success() {
        Liveness::Alive
    } else if status.as_u16() == 404 || status.as_u16() == 400 {
        Liveness::Dead
    } else {
        Liveness::Unknown
    }
}

/// HTML-ответ = без-cookie заглушка гейта (Caddy @nologin рерайтит любой
/// путь в страницу «Войти», #1160): content-type text/html; при
/// отсутствии классифицирующего заголовка — doctype-снифф начала тела.
/// Детектор вызывается в call() — единственной точке парсинга — поэтому
/// накрывает всех потребителей трейта; HTML-ответ = отказ пробы (Err),
/// не «факт» о сессиях.
fn html_gate_response(content_type: Option<&str>, body: &str) -> bool {
    if content_type.is_some_and(|ct| ct.to_ascii_lowercase().contains("text/html")) {
        return true;
    }
    // снифф — только когда заголовок не классифицировал ответ (middlebox
    // без content-type): иначе false positive на легитимный plain-text,
    // начинающийся с HTML-документа (Low ревью #1160; заглушка гейта
    // всегда несёт text/html — живая проба 18.11)
    if content_type.is_some() {
        return false;
    }
    let trimmed = body.trim_start();
    starts_with_ci(trimmed, b"<!doctype html") || starts_with_ci(trimmed, b"<html")
}

/// Регистронезависимый префикс первых байтов — без копии тела целиком
/// (Low ревью #1160: to_ascii_lowercase на каждом ответе — лишняя
/// аллокация больших страниц списка на каждом тике).
fn starts_with_ci(haystack: &str, needle: &[u8]) -> bool {
    let bytes = haystack.as_bytes();
    bytes.len() >= needle.len()
        && bytes[..needle.len()]
            .iter()
            .zip(needle)
            .all(|(b, n)| b.to_ascii_lowercase() == *n)
}

impl OpencodeClient {
    /// gate_cookie — значение Cookie-хедера целиком (oc_gate=...):
    /// потребляется в дефолтные хедеры клиента, полем не хранится.
    pub fn new(base: String, password: String, gate_cookie: Option<String>) -> Self {
        let mut headers = reqwest::header::HeaderMap::new();
        if let Some(cookie) = &gate_cookie {
            let value = reqwest::header::HeaderValue::from_str(cookie).expect("gate cookie: ascii");
            headers.insert(reqwest::header::COOKIE, value);
        }
        Self {
            base: base.trim_end_matches('/').to_string(),
            password,
            http: reqwest::Client::builder()
                .default_headers(headers)
                .timeout(Duration::from_secs(20))
                .build()
                .expect("reqwest client"),
            model_limits: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// call: маршруты клиента. Бюджет (Option<u64>) — per-request таймаут
    /// в миллисекундах, переопределяет общий клиентский на этот запрос:
    /// синхронные команды env-gc (#1526) несут бюджет прохода — ответ
    /// приходит после полного завершения shell-блока; None — общий
    /// клиентский таймаут. Литерал пути живёт на сайте вызова — канарейка
    /// route-пина (#1329) сверяет их с манифестом мока.
    async fn call(
        &self,
        budget_ms: Option<u64>,
        path: &str,
        method: reqwest::Method,
        body: Option<Value>,
    ) -> UsecaseResult<(reqwest::StatusCode, Value)> {
        let mut req = self
            .http
            .request(method, format!("{}/api{path}", self.base))
            .basic_auth("opencode", Some(&self.password));
        if let Some(ms) = budget_ms {
            req = req.timeout(std::time::Duration::from_millis(ms));
        }
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| UsecaseError::Session(format!("transport {path}: {e}")))?;
        let status = resp.status();
        // content-type в owned-строку до text() (он потребляет resp)
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let text = resp
            .text()
            .await
            .map_err(|e| UsecaseError::Session(format!("body {path}: {e}")))?;
        // HTML-ответ гейта (без-cookie заглушка «Войти») — отказ пробы,
        // не данные: молчаливый Null здесь давал Ok(пусто) в списке
        // сессий, ложный Alive/Dead в liveness и «успешную» недоставку
        // промптов с записью seen-маркера (потеря комментов) — #1160
        if html_gate_response(content_type.as_deref(), &text) {
            return Err(UsecaseError::Session(format!(
                "gate HTML-заглушка на {path} (без-cookie ответ гейта?)"
            )));
        }
        let value = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::Null)
        };
        Ok((status, value))
    }

    fn check(status: reqwest::StatusCode, what: &str) -> UsecaseResult<()> {
        if status.is_success() {
            Ok(())
        } else {
            Err(UsecaseError::Session(format!("{what}: HTTP {status}")))
        }
    }

    /// Классификация session-scoped доставки (#1288): 404/400 — dead-session
    /// (та же карта статусов, что `liveness_of_status`), прочие неуспехи —
    /// временный класс. Финальность видна в типе, текст несёт класс агенту
    /// (gherr-стиль ADR-110).
    fn check_session(status: reqwest::StatusCode, what: &str) -> UsecaseResult<()> {
        if status.is_success() {
            return Ok(());
        }
        let detail = format!("{what}: HTTP {status}");
        if status.as_u16() == 404 || status.as_u16() == 400 {
            Err(UsecaseError::SessionNotFound(detail))
        } else {
            Err(UsecaseError::Session(detail))
        }
    }
}

/// Тело POST /session из спека. Контракт моделей (#1154): модель —
/// карта сервера, кладётся в тело всегда (body.model перекрывает деф
/// агента — деф-файлы моделей не несут; решение владельца 17.11).
fn create_session_body(spec: &SessionSpec) -> Value {
    let mut body = json!({
        "title": spec.title.chars().take(64).collect::<String>(),
        "location": { "directory": spec.directory },
    });
    if let Some(name) = &spec.custom_agent {
        // именованный агент опенкода (agents/<name>.md в cwd):
        // боты с fm agent — методичка вшита системой (#349)
        body["agent"] = json!(name);
    } else {
        match &spec.agent {
            uwuwu_domain::AgentSlot::Role(agent) => {
                body["agent"] = json!(uwuwu_usecase::agent_name(*agent));
            }
            uwuwu_domain::AgentSlot::Default => {}
        }
    }
    let mut m = json!({ "providerID": spec.model.provider(), "id": spec.model.id() });
    if let Some(v) = &spec.variant {
        m["variant"] = json!(v);
    }
    body["model"] = m;
    body
}

#[async_trait]
impl AgentSessionGateway for OpencodeClient {
    async fn create_session(&self, spec: &SessionSpec) -> UsecaseResult<String> {
        let body = create_session_body(spec);
        let (status, resp) = self
            .call(None, "/session", reqwest::Method::POST, Some(body))
            .await?;
        Self::check(status, "create_session")?;
        let id = resp["data"]["id"]
            .as_str()
            .or(resp["id"].as_str())
            .unwrap_or_default()
            .to_string();
        if id.is_empty() {
            return Err(UsecaseError::Session("create_session: пустой id".into()));
        }
        Ok(id)
    }

    async fn prompt(&self, session: &str, text: &str) -> UsecaseResult<()> {
        let (status, _) = self
            .call(
                None,
                &format!("/session/{session}/prompt"),
                reqwest::Method::POST,
                Some(json!({ "text": text })),
            )
            .await?;
        Self::check_session(status, &format!("prompt {session}"))
    }

    /// Пин агента слота (#1300): `POST /session/:id/agent` — живая проба
    /// 2.3.2 (throwaway serve): 204, session.agent персистится туда-обратно.
    async fn switch_agent(&self, session: &str, agent: uwuwu_domain::Agent) -> UsecaseResult<()> {
        let (status, _) = self
            .call(
                None,
                &format!("/session/{session}/agent"),
                reqwest::Method::POST,
                Some(json!({ "agent": uwuwu_usecase::agent_name(agent) })),
            )
            .await?;
        // 404 = сессия умерла между liveness-пробой и пином — честный
        // классифицированный Err (#1288): seen не пишется, маршрутизация
        // потери на тике
        Self::check_session(status, &format!("switch agent {session}"))
    }

    /// Slash-команда сессии. POST /command синхронен (проба #1526): ответ
    /// после полного завершения shell-блока, поэтому per-request таймаут
    /// = budget_ms от юзкейса (батч env-gc — 15 мин); общий клиентский
    /// таймаут не трогается — зависшие GET/промпты других контуров
    /// остаются на 20с.
    async fn run_command(
        &self,
        session: &str,
        name: &str,
        args: &str,
        budget_ms: u64,
    ) -> UsecaseResult<()> {
        let (status, _) = self
            .call(
                Some(budget_ms),
                &format!("/session/{session}/command"),
                reqwest::Method::POST,
                Some(json!({ "name": name, "text": args })),
            )
            .await?;
        Self::check(status, &format!("command {session}/{name}"))
    }

    async fn liveness(&self, session: &str) -> Liveness {
        match self
            .call(
                None,
                &format!("/session/{session}"),
                reqwest::Method::GET,
                None,
            )
            .await
        {
            Ok((status, _)) => liveness_of_status(status),
            Err(_) => Liveness::Unknown,
        }
    }

    async fn delete_session(&self, session: &str) -> UsecaseResult<()> {
        let (status, _) = self
            .call(
                None,
                &format!("/session/{session}"),
                reqwest::Method::DELETE,
                None,
            )
            .await?;
        // 404 = уже нет — идемпотентно ок
        if status.is_success() || status.as_u16() == 404 {
            Ok(())
        } else {
            Err(UsecaseError::Session(format!(
                "delete {session}: HTTP {status}"
            )))
        }
    }

    async fn abort_session(&self, session: &str) -> UsecaseResult<()> {
        // Остановка хода для преемпшена (#1259). Живая проба бинаря
        // 2.3.0 (18.11, throwaway serve): POST /session/:id/abort даёт
        // 404 при живом ходе (тихий no-op недопустим), рабочий путь —
        // interrupt: ход остановлен, busy снят. Фоллбек abort→interrupt
        // покрывает обе раскладки билдов; 404 на interrupt = сессия уже
        // погашена между сканом и абортом (идемпотентно), прочие
        // неуспехи → Err: преемпшен остаётся held, ретрай следующим
        // тиком. Источник факта — живая проба 18.11 (задача #1259);
        // санкция владельца на выбор пути запрошена в треде задачи и на
        // момент коммита не дана — при «чистый interrupt» фоллбек
        // снимается (две строки).
        if let Ok((status, _)) = self
            .call(
                None,
                &format!("/session/{session}/abort"),
                reqwest::Method::POST,
                None,
            )
            .await
        {
            if status.is_success() {
                return Ok(());
            }
        }
        match self
            .call(
                None,
                &format!("/session/{session}/interrupt"),
                reqwest::Method::POST,
                None,
            )
            .await
        {
            Ok((status, _)) if status.is_success() => Ok(()),
            Ok((status, _)) if status.as_u16() == 404 => Ok(()),
            Ok((status, _)) => Err(UsecaseError::Session(format!(
                "abort {session}: HTTP {status}"
            ))),
            Err(e) => Err(e),
        }
    }

    async fn list_sessions(&self) -> UsecaseResult<Vec<SessionInfo>> {
        // Пагинация (#1163 ревью Common-1): список пагинирован newest-first —
        // слепая зона «новейших-100» прятала ходы в полёте с тишиной
        // провайдера (#1150: 72 мин без движения time.updated) от second
        // source и env-гарда: churn спавнов кладёт новые сессии наверх.
        const PAGE_LIMIT: usize = 1000;
        const MAX_PAGES: usize = 100;
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut url = format!("/session?limit={PAGE_LIMIT}");
            if let Some(c) = &cursor {
                url.push_str(&format!("&cursor={c}"));
            }
            let (status, resp) = self.call(None, &url, reqwest::Method::GET, None).await?;
            Self::check(status, "list sessions")?;
            let (page, next) = parse_session_page(&resp, PAGE_LIMIT);
            out.extend(page);
            let Some(next) = next else {
                return Ok(out);
            };
            cursor = Some(next);
        }
        Err(UsecaseError::Session(format!(
            "list sessions: пагинация не сошлась за {MAX_PAGES} страниц — список искажён, тик пропустит"
        )))
    }

    async fn export_session(&self, session: &str) -> UsecaseResult<String> {
        let (status, resp) = self
            .call(
                None,
                &format!("/experimental/session/{session}/export"),
                reqwest::Method::GET,
                None,
            )
            .await?;
        Self::check(status, &format!("export {session}"))?;
        // экспорт может быть data-обёрнут или plain-текстом
        if let Some(text) = resp["data"].as_str() {
            return Ok(text.to_string());
        }
        if let Some(text) = resp.as_str() {
            return Ok(text.to_string());
        }
        Ok(resp.to_string())
    }

    async fn last_activity_ms(&self, session: &str) -> UsecaseResult<Option<u64>> {
        let (status, resp) = self
            .call(
                None,
                &format!("/session/{session}/message?limit=1&order=desc"),
                reqwest::Method::GET,
                None,
            )
            .await?;
        Self::check(status, &format!("message tail {session}"))?;
        // обе формы ответа: массив и {data:[...]} (opencode 2.3) — иначе
        // вечное Ok(None) гасит сталл-детект (инцидент 17.11)
        Ok(message_array(&resp)
            .and_then(|msgs| msgs.first())
            .and_then(message_activity_ms))
    }

    /// Форма хвоста сверена живой пробой опенкода 2.3.0 (задача #1150):
    /// сообщения несут `type` («user»|«assistant»|…), у ассистента —
    /// `content[].text`, `finish` и `time.created`; лимит 5 — в хвосте
    /// могут висеть служебные события (idle) поверх ответа.
    async fn last_assistant_reply(&self, session: &str) -> UsecaseResult<Option<AssistantReply>> {
        let (status, resp) = self
            .call(
                None,
                &format!("/session/{session}/message?limit=5&order=desc"),
                reqwest::Method::GET,
                None,
            )
            .await?;
        Self::check(status, &format!("message tail {session}"))?;
        let msgs = message_array(&resp);
        // Самый свежий СОДЕРЖАТЕЛЬНЫЙ блок журнала (order=desc): служебные
        // события поверх финального ответа (#1150: «в хвосте могут висеть
        // idle») не делают завершённую сессию busy — пропускаем их
        let latest_is_assistant = msgs
            .map(|m| latest_meaningful_is_assistant(m))
            .unwrap_or(false);
        let Some(msg) = msgs.and_then(|m| m.iter().find(|m| m["type"] == "assistant")) else {
            return Ok(None);
        };
        let text: Vec<&str> = msg["content"]
            .as_array()
            .map(|parts| {
                parts
                    .iter()
                    .filter(|p| p["type"] == "text")
                    .filter_map(|p| p["text"].as_str())
                    .collect()
            })
            .unwrap_or_default();
        Ok(Some(AssistantReply {
            created_ms: msg["time"]["created"].as_u64().unwrap_or_default(),
            text: text.join("\n"),
            finished: msg["finish"].as_str().is_some(),
            is_latest: latest_is_assistant,
            finish: msg["finish"].as_str().map(String::from),
        }))
    }

    /// Прямой shell без LLM-поворота (#1526: вырезаем slash-команду
    /// task-env-gc — её user-сообщение «ENV-GC … / Служебная команда…»
    /// плодило мусорные сессии в листинге исполнителя и жгло поворот на
    /// проход). POST /session/:id/shell синхронен (204 после завершения
    /// shell-блока), per-request таймаут = budget_ms; итог — из хвоста
    /// ленты: первое завершённое shell-сообщение своей команды.
    /// None — за бюджет завершённого shell-сообщения нет. Err наружу не
    /// выходит: транспортные сбои ретраятся внутри до дедлайна.
    async fn run_shell(
        &self,
        session: &str,
        command: &str,
        budget_ms: u64,
    ) -> UsecaseResult<Option<String>> {
        let (status, _) = self
            .call(
                Some(budget_ms),
                &format!("/session/{session}/shell"),
                reqwest::Method::POST,
                Some(json!({ "command": command })),
            )
            .await?;
        Self::check(status, &format!("shell {session}"))?;
        let started = std::time::Instant::now();
        loop {
            match self.last_shell_output(session, command).await {
                Ok(Some(text)) => return Ok(Some(text)),
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(
                        event = "env.shell_poll_retry",
                        session,
                        error = %e,
                        "Опрос ленты упал — ретрай до дедлайна"
                    );
                }
            }
            if started.elapsed().as_millis() as u64 >= budget_ms {
                return Ok(None);
            }
            tokio::time::sleep(USER_MSG_POLL).await;
        }
    }

    /// Занятость окна контекста (#1221): токены свежайшего assistant-
    /// поворота против лимита модели из каталога исполнителя. Сбои
    /// хвоста — Err; «не измерить» (нет поворотов / нет лимита) —
    /// Ok(None), не стреляем.
    async fn context_usage_pct(
        &self,
        session: &str,
        model: &ModelRef,
    ) -> UsecaseResult<Option<f64>> {
        let (status, resp) = self
            .call(
                None,
                &format!("/session/{session}/message?limit=10&order=desc"),
                reqwest::Method::GET,
                None,
            )
            .await?;
        Self::check(status, &format!("message tail {session}"))?;
        let Some(tokens) = message_array(&resp).and_then(|m| newest_assistant_tokens(m)) else {
            return Ok(None);
        };
        let Some(limit) = self.model_context_limit(model).await? else {
            return Ok(None);
        };
        Ok(Some(tokens as f64 / limit as f64 * 100.0))
    }

    /// POST /session/:id/compact (#1221, ADR-082): durable admission —
    /// результат (compaction-сообщение в хвосте) асинхронный, Ok —
    /// заявка принята исполнителем.
    async fn compact_session(&self, session: &str) -> UsecaseResult<()> {
        let (status, _) = self
            .call(
                None,
                &format!("/session/{session}/compact"),
                reqwest::Method::POST,
                Some(serde_json::json!({})),
            )
            .await?;
        Self::check(status, &format!("compact {session}"))
    }
}

impl OpencodeClient {
    /// Вывод завершённого shell-сообщения своей команды из хвоста ленты;
    /// None — такого сообщения нет (ещё в полёте / чужое). Форма живого
    /// носителя (opencode 2.3.2, живая проба #1526): type shell несёт
    /// command/status/output.output; user-сообщений команда не создаёт.
    async fn last_shell_output(
        &self,
        session: &str,
        command: &str,
    ) -> UsecaseResult<Option<String>> {
        let (status, resp) = self
            .call(
                None,
                &format!("/session/{session}/message?limit=10&order=desc"),
                reqwest::Method::GET,
                None,
            )
            .await?;
        Self::check(status, &format!("message tail {session}"))?;
        Ok(
            message_array(&resp)
                .and_then(|msgs| msgs.iter().find_map(|m| shell_output(m, command))),
        )
    }

    /// Лимит окна контекста модели из каталога исполнителя GET /model
    /// (клиент клеит /api — строгий pathname-тест стережёт двойной
    /// префикс, живой урок ADR-082). Кэш per-client: Ok(None)
    /// перманентного отсутствия кэшируется навсегда, Err (transport/
    /// HTTP) — не кэшируется.
    async fn model_context_limit(&self, model: &ModelRef) -> UsecaseResult<Option<u64>> {
        let key = format!("{}/{}", model.provider(), model.id());
        if let Some(cached) = self.model_limits.lock().expect("limits lock").get(&key) {
            return Ok(*cached);
        }
        let (status, resp) = self
            .call(None, "/model", reqwest::Method::GET, None)
            .await?;
        Self::check(status, "model catalog")?;
        let empty = Vec::new();
        let items = resp
            .as_array()
            .or_else(|| resp["data"].as_array())
            .unwrap_or(&empty);
        let mut resolved = None;
        for item in items {
            if item["providerID"] == json!(model.provider()) && item["modelID"] == json!(model.id())
            {
                resolved = item["limit"]["context"].as_u64().filter(|v| *v > 0);
                break;
            }
        }
        self.model_limits
            .lock()
            .expect("limits lock")
            .insert(key, resolved);
        Ok(resolved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use uwuwu_domain::{Agent, AgentSlot, ModelRef};

    /// Мульти-запросный стаб (#1221): canned-ответы по одному на
    /// соединение, каждый (METHOD path) записывается — строгий pathname-
    /// матч стережёт класс двойного /api-префикса (ADR-082, a67a8505).
    fn stub_multi(responses: Vec<(&'static str, String)>) -> (String, Arc<Mutex<Vec<String>>>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let hits: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let queue = Arc::new(Mutex::new(responses));
        let hits2 = hits.clone();
        std::thread::spawn(move || loop {
            let (status, body) = {
                let mut q = queue.lock().expect("queue lock");
                if q.is_empty() {
                    return;
                }
                q.remove(0)
            };
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut data = Vec::new();
            let mut buf = [0u8; 2048];
            loop {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => data.extend_from_slice(&buf[..n]),
                }
                if let Some(pos) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                    let cl: usize = data[..pos]
                        .split(|b| *b == b'\n')
                        .find_map(|line| {
                            String::from_utf8_lossy(line)
                                .to_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse().ok())
                        })
                        .unwrap_or(0);
                    if data.len() >= pos + 4 + cl {
                        break;
                    }
                }
            }
            let request_line =
                String::from_utf8_lossy(data.split(|b| *b == b'\n').next().unwrap_or(&[]))
                    .to_string();
            // версия HTTP отрезается: ассерты матчают METHOD path (строгий
            // pathname — класс двойного /api-префикса)
            let recorded = request_line
                .trim_end()
                .strip_suffix(" HTTP/1.1")
                .unwrap_or_else(|| request_line.trim_end());
            hits2.lock().expect("hits lock").push(recorded.to_string());
            let resp = format!(
                "{status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes());
        });
        (format!("http://127.0.0.1:{port}"), hits)
    }

    fn spec(agent: AgentSlot, model: ModelRef, variant: Option<&str>) -> SessionSpec {
        SessionSpec {
            directory: "/tmp".into(),
            title: "t".into(),
            agent,
            custom_agent: None,
            model,
            variant: variant.map(str::to_string),
            bootstrap: false,
        }
    }

    #[test]
    fn body_always_carries_model_from_server_map() {
        // #1154 (решение владельца 17.11): модель — карта сервера, тело
        // спавна несёт её всегда (деф-файлы моделей не несут)
        let with = create_session_body(&spec(
            AgentSlot::Role(Agent::Dreamer),
            ModelRef::new("opencode-go", "hy4-flash-free"),
            Some("high"),
        ));
        assert_eq!(with["agent"], "dreamer");
        assert_eq!(with["model"]["providerID"], "opencode-go");
        assert_eq!(with["model"]["id"], "hy4-flash-free");
        assert_eq!(with["model"]["variant"], "high");

        let without = create_session_body(&spec(
            AgentSlot::Default,
            ModelRef::new("zai-coding-plan", "glm-5.4"),
            None,
        ));
        assert!(without.get("agent").is_none(), "Default — без agent");
        assert_eq!(
            without["model"]["id"], "glm-5.4",
            "модель в теле всегда (карта сервера)"
        );
    }

    #[test]
    fn message_array_takes_both_response_shapes() {
        // 17.11: opencode 2.3 отдаёт {data:[...]} — last_activity_ms знал
        // только голый массив, вечное Ok(None) гасил сталл-детект
        let bare = serde_json::json!([{ "type": "assistant" }]);
        let wrapped = serde_json::json!({ "data": [{ "type": "assistant" }] });
        assert_eq!(message_array(&bare).map(Vec::len), Some(1));
        assert_eq!(message_array(&wrapped).map(Vec::len), Some(1));
        assert!(message_array(&serde_json::json!({})).is_none());
    }

    #[test]
    fn shell_output_reads_live_carrier_shape() {
        // Живая форма shell-сообщения opencode 2.3.2 (throwaway-проба
        // #1526: POST shell → 204, в ленте type shell с output.output,
        // user-сообщений и assistant-поворота нет, токены 0):
        // завершённое сообщение своей команды — текст вывода, чужое и
        // running — None
        let cmd = "$HOME/uwuwu-agent/server/ops/task-env-gc.sh sweep 1431 1432";
        let live = serde_json::json!({
            "id": "msg_x", "type": "shell",
            "time": { "created": 1795587829146u64, "completed": 1795587829630u64 },
            "shellID": "sh_x", "command": cmd,
            "status": "exited", "exit": 0,
            "output": { "output": "ENV-GC 1431 SWEPT\nENV-GC 1432 ABSENT\n", "cursor": 34, "size": 34, "truncated": false }
        });
        assert_eq!(
            shell_output(&live, cmd).as_deref(),
            Some("ENV-GC 1431 SWEPT\nENV-GC 1432 ABSENT\n"),
            "живая форма: output завершённого shell своей команды"
        );
        let running = serde_json::json!({
            "id": "msg_y", "type": "shell", "command": cmd,
            "status": "running", "output": { "output": "", "cursor": 0, "size": 0, "truncated": false }
        });
        assert!(
            shell_output(&running, cmd).is_none(),
            "running — исхода нет"
        );
        let foreign = serde_json::json!({
            "id": "msg_z", "type": "shell", "command": "other",
            "status": "exited", "exit": 0,
            "output": { "output": "x", "cursor": 1, "size": 1, "truncated": false }
        });
        assert!(
            shell_output(&foreign, cmd).is_none(),
            "чужая команда — не наш вывод"
        );
        // user-сообщения env-gc больше не существует: slash-команда
        // вырезана (#1526 — мусорные сессии «ENV-GC … / Служебная
        // команда…» в листинге исполнителя)
        let user =
            serde_json::json!({ "id": "msg_u", "type": "user", "text": "ENV-GC 1431 SWEPT" });
        assert!(
            shell_output(&user, cmd).is_none(),
            "user-сообщение — не shell-вывод"
        );
    }

    #[test]
    fn zombie_turn_stops_holding_slot_when_updates_stop() {
        // #409/#1547: оборванный ход держит busy > idle навсегда, но
        // перестаёт обновляться. Свежая сессия держит слот; протухшая — нет.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let fresh = serde_json::json!({
            "id": "s1", "agent": "developer",
            "time": {
                "busy": now - 60_000, "idle": now - 120_000,
                "updated": now - 30_000
            }
        });
        let stale = serde_json::json!({
            "id": "s2", "agent": "developer",
            "time": {
                "busy": now - 31 * 60_000, "idle": now - 60 * 60_000,
                "updated": now - 31 * 60_000
            }
        });
        let idle_cleared = serde_json::json!({
            "id": "s3", "agent": "developer",
            "time": {
                "busy": now - 60 * 60_000, "idle": now - 5 * 60_000,
                "updated": now - 30_000
            }
        });
        assert!(session_busy(&fresh, now), "свежий ход — busy");
        assert!(!session_busy(&stale, now), "зомби-ход молчит — не busy");
        assert!(
            !session_busy(&idle_cleared, now),
            "idle новее busy — не busy"
        );
    }

    #[test]
    fn long_running_turn_holds_slot_while_session_keeps_updating() {
        // #1547: живой ход длиннее 30 минут держит слот, пока лента обновляется.
        // Возраст time.busy сам по себе не означает оборванный ход.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let long_live = serde_json::json!({
            "id": "s1", "agent": "developer",
            "time": {
                "busy": now - 35 * 60_000,
                "idle": now - 36 * 60_000,
                "updated": now - 30_000
            }
        });
        assert!(session_busy(&long_live, now), "длинный живой ход — busy");
    }

    #[test]
    fn message_activity_reads_live_203_shape() {
        // живой прод-ответ 17.11 (сессия #1202, хвост order=desc):
        // idle-событие несёт только time.created, assistant —
        // created/firstToken/streamed/completed; time.updated НЕТ —
        // чтение по updated давало вечное None, гася сталл-детект
        let idle = serde_json::json!({
            "id": "msg_a", "type": "idle", "outcome": "succeeded",
            "time": { "created": 1794872146623u64 }
        });
        assert_eq!(message_activity_ms(&idle), Some(1794872146623));
        let assistant = serde_json::json!({
            "id": "msg_b", "type": "assistant",
            "time": {
                "created": 1794872138576u64, "firstToken": 1794872139843u64,
                "streamed": 1794872146589u64, "completed": 1794872146593u64
            }
        });
        assert_eq!(message_activity_ms(&assistant), Some(1794872146593));
        // легаси-форма с updated — по-прежнему читается
        let legacy = serde_json::json!({ "time": { "updated": 42u64, "created": 1u64 } });
        assert_eq!(message_activity_ms(&legacy), Some(42));
    }

    #[test]
    fn latest_meaningful_skips_service_events() {
        // #1206 Common-1: служебные события (idle и пр.) поверх финального
        // ответа не делают завершённую сессию busy
        let tail = serde_json::json!([
            { "type": "idle" },
            { "type": "assistant", "finish": "stop" }
        ]);
        assert!(latest_meaningful_is_assistant(tail.as_array().unwrap()));
        // сверху промпт — сессия занята обработкой
        let tail = serde_json::json!([
            { "type": "user" },
            { "type": "assistant", "finish": "stop" }
        ]);
        assert!(!latest_meaningful_is_assistant(tail.as_array().unwrap()));
        // только служебные — данных нет, консервативно busy
        let tail = serde_json::json!([{ "type": "idle" }]);
        assert!(!latest_meaningful_is_assistant(tail.as_array().unwrap()));
    }

    #[test]
    fn liveness_maps_statuses() {
        assert_eq!(liveness_of_status(reqwest::StatusCode::OK), Liveness::Alive);
        assert_eq!(
            liveness_of_status(reqwest::StatusCode::NOT_FOUND),
            Liveness::Dead
        );
        assert_eq!(
            liveness_of_status(reqwest::StatusCode::BAD_REQUEST),
            Liveness::Dead
        );
        assert_eq!(
            liveness_of_status(reqwest::StatusCode::BAD_GATEWAY),
            Liveness::Unknown
        );
    }

    #[test]
    fn session_scoped_delivery_classifies_dead_vs_transient() {
        // #1288: 404/400 — финальный dead-session класс (карта liveness);
        // прочие неуспехи — временный Session, ретрай легитимен
        use uwuwu_usecase::UsecaseError;
        assert!(OpencodeClient::check_session(reqwest::StatusCode::OK, "prompt s").is_ok());
        let lost =
            OpencodeClient::check_session(reqwest::StatusCode::NOT_FOUND, "prompt s").unwrap_err();
        assert!(matches!(lost, UsecaseError::SessionNotFound(_)));
        let lost400 = OpencodeClient::check_session(reqwest::StatusCode::BAD_REQUEST, "prompt s")
            .unwrap_err();
        assert!(matches!(lost400, UsecaseError::SessionNotFound(_)));
        let transient = OpencodeClient::check_session(reqwest::StatusCode::BAD_GATEWAY, "prompt s")
            .unwrap_err();
        assert!(
            matches!(transient, UsecaseError::Session(_)),
            "не 404/400 — временный класс, не dead-session"
        );
        // класс виден агенту в тексте (gherr-стиль ADR-110)
        assert!(lost.to_string().contains("dead-session"));
    }

    #[test]
    fn html_gate_response_detects_content_type_and_body_sniff() {
        // #1160: text/html в заголовке — детект независимо от тела
        assert!(html_gate_response(Some("text/html; charset=utf-8"), ""));
        // снифф тела без заголовка: doctype и <html>, case-insensitive
        assert!(html_gate_response(None, "  <!DOCTYPE HTML><html>"));
        assert!(html_gate_response(None, "<HTML>\n"));
        // JSON / пустое тело / plain-text — не HTML (легитимные пути)
        assert!(!html_gate_response(Some("application/json"), "{\"a\":1}"));
        assert!(!html_gate_response(None, ""));
        assert!(!html_gate_response(None, "502 Bad Gateway"));
        // Low дельты #1160: классифицирующий заголовок отключает снифф —
        // plain-text (экспорт), начинающийся с HTML-документа, не Err
        assert!(!html_gate_response(
            Some("text/plain"),
            "<!doctype html><html><body>экспорт</body></html>"
        ));
    }

    fn page_item(id: &str, directory: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "agent": null,
            "location": { "directory": directory },
            "time": { "created": 1, "updated": 2 },
            "outcome": null,
            "tokens": { "input": 3, "output": 4 }
        })
    }

    fn parsed_with_time(busy: serde_json::Value, idle: serde_json::Value) -> bool {
        let mut item = page_item("s", "/x");
        item["time"]["busy"] = busy;
        item["time"]["idle"] = idle;
        item["time"]["updated"] = serde_json::json!(std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64);
        parse_session_page(&serde_json::json!({ "data": [item] }), 1)
            .0
            .remove(0)
            .busy
    }

    #[test]
    fn session_page_busy_semantics() {
        // #1231: busy ⟺ time.busy > time.idle — ход в полёте (Started
        // пишет time.busy, idle-финал пишет time.idle). outcome — атрибут
        // последнего завершённого хода, живость не выражает.
        // PR #409: плюс зомби-гард — флаг busy свежее 30 мин (фикстуры
        // динамические от now: статичные ms протухают по определению).
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        // ход в полёте: Started есть (минуту назад), idle ещё не ставился
        assert!(parsed_with_time(
            serde_json::json!(now - 60_000),
            serde_json::Value::Null
        ));
        // завершённый ход: idle позже Started — слот свободен
        assert!(!parsed_with_time(
            serde_json::json!(now - 120_000),
            serde_json::json!(now - 60_000)
        ));
        // корень бага #1231: второй ход поверх финала (outcome непуст) —
        // Started позже последнего idle, сессия держит кап
        assert!(parsed_with_time(
            serde_json::json!(now - 60_000),
            serde_json::json!(now - 3_600_000)
        ));
        // без события Started — не busy (ручная/легаси-сессия)
        assert!(!parsed_with_time(
            serde_json::Value::Null,
            serde_json::json!(now - 60_000)
        ));
        // событий нет вовсе — не busy (page_item без time.busy/idle)
        assert!(!parsed_with_time(
            serde_json::Value::Null,
            serde_json::Value::Null
        ));
    }

    #[test]
    fn session_page_full_page_yields_next_cursor() {
        // полная страница + непустой cursor.next → продолжение пагинации
        let resp = serde_json::json!({
            "data": [page_item("a", "/x"), page_item("b", "/y")],
            "cursor": { "next": "c2" }
        });
        let (page, next) = parse_session_page(&resp, 2);
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].directory, "/x");
        assert_eq!(page[0].total_tokens, 7);
        assert_eq!(next.as_deref(), Some("c2"));
    }

    #[test]
    fn session_page_partial_page_terminates_even_with_cursor() {
        // неполная страница — конец: opencode ставит cursor.next и на
        // исчерпанном списке (живая проба 17.11)
        let resp = serde_json::json!({
            "data": [page_item("a", "/x")],
            "cursor": { "next": "stale-cursor" }
        });
        let (page, next) = parse_session_page(&resp, 2);
        assert_eq!(page.len(), 1);
        assert_eq!(next, None);
    }

    #[test]
    fn session_page_cursor_shapes() {
        // пустой/отсутствующий курсор на полной странице — конец
        let empty_next = serde_json::json!({
            "data": [page_item("a", "/x")],
            "cursor": { "next": "" }
        });
        assert_eq!(parse_session_page(&empty_next, 1).1, None);
        let no_cursor = serde_json::json!({ "data": [page_item("a", "/x")] });
        assert_eq!(parse_session_page(&no_cursor, 1).1, None);
        // data-обёртка отсутствует (plain array) — тоже парсится
        let plain = serde_json::json!([page_item("a", "/x")]);
        assert_eq!(parse_session_page(&plain, 1).0.len(), 1);
        // сессия без location — вне раскладки (пустая директория)
        let no_loc = serde_json::json!([serde_json::json!({ "id": "z" })]);
        let (page, _) = parse_session_page(&no_loc, 1);
        assert_eq!(page[0].directory, "");
    }

    #[test]
    fn base_trims_trailing_slash() {
        let c = OpencodeClient::new("https://x.y/".into(), "p".into(), None);
        assert_eq!(c.base, "https://x.y");
    }

    #[test]
    fn gate_cookie_accepted() {
        // кука с ascii-значением проходит в хедеры без паники
        let c = OpencodeClient::new(
            "https://x.y".into(),
            "p".into(),
            Some("oc_gate=g77a99dcff9257cb30992ef970d7e8d1668d1130f".into()),
        );
        assert_eq!(c.base, "https://x.y");
    }

    /// Стаб-сервер: один canned-ответ на любой запрос (TcpListener в
    /// потоке, без новых дев-депов — прецедент tg.rs). Запрос читается
    /// целиком (заголовки + тело по Content-Length) — иначе ранний close
    /// даёт RST и reqwest падает на отправке.
    fn stub_server(status: &'static str, content_type: &'static str, body: &'static str) -> String {
        use std::io::Read;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            use std::io::Write;
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut data = Vec::new();
            let mut buf = [0u8; 2048];
            loop {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => data.extend_from_slice(&buf[..n]),
                }
                if let Some(pos) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                    let cl: usize = data[..pos]
                        .split(|b| *b == b'\n')
                        .find_map(|line| {
                            String::from_utf8_lossy(line)
                                .to_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse().ok())
                        })
                        .unwrap_or(0);
                    if data.len() >= pos + 4 + cl {
                        break;
                    }
                }
            }
            let resp = format!(
                "{status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes());
        });
        format!("http://127.0.0.1:{port}")
    }

    const GATE_STUB_BODY: &str =
        "<!doctype html><html lang=\"ru\"><title>OpenCode — доступ</title></html>";

    #[tokio::test]
    async fn list_sessions_gate_html_stub_is_error_not_empty_list() {
        // #1160: без-cookie заглушка гейта (200 + text/html) сегодня
        // молча даёт Ok(пусто) — CUP видит 0, гард спавна проходит
        // насквозь; контракт: Err (strict-скан → hold)
        let base = stub_server(
            "HTTP/1.1 200 OK",
            "text/html; charset=utf-8",
            GATE_STUB_BODY,
        );
        let c = OpencodeClient::new(base, "p".into(), None);
        let res = c.list_sessions().await;
        assert!(
            res.is_err(),
            "HTML-заглушка не должна выглядеть как пустой список: {res:?}"
        );
    }

    #[tokio::test]
    async fn liveness_gate_html_200_stub_is_unknown_not_alive() {
        // #1160: 200-HTML-проба — «проба не удалась», не Alive
        let base = stub_server(
            "HTTP/1.1 200 OK",
            "text/html; charset=utf-8",
            GATE_STUB_BODY,
        );
        let c = OpencodeClient::new(base, "p".into(), None);
        assert_eq!(c.liveness("ses_x").await, Liveness::Unknown);
    }

    #[tokio::test]
    async fn liveness_gate_html_404_stub_is_unknown_not_dead() {
        // #1160: 404-вариант заглушки (файл index.html отсутствует на VPS)
        // — прямой симптом «Dead на живых сессиях»; контракт: Unknown
        let base = stub_server(
            "HTTP/1.1 404 Not Found",
            "text/html; charset=utf-8",
            GATE_STUB_BODY,
        );
        let c = OpencodeClient::new(base, "p".into(), None);
        assert_eq!(c.liveness("ses_x").await, Liveness::Unknown);
    }

    #[tokio::test]
    async fn prompt_gate_html_stub_is_error_delivery_retries_next_tick() {
        // #1160: «успешный» промпт на заглушке писал seen-маркер — коммент
        // терялся навсегда; контракт: Err (seen не пишется, повтор доставки)
        let base = stub_server(
            "HTTP/1.1 200 OK",
            "text/html; charset=utf-8",
            GATE_STUB_BODY,
        );
        let c = OpencodeClient::new(base, "p".into(), None);
        let res = c.prompt("ses_x", "Wakeup").await;
        assert!(
            res.is_err(),
            "HTML-заглушка не должна выглядеть как доставка: {res:?}"
        );
    }

    #[tokio::test]
    async fn liveness_json_404_stays_dead() {
        // регресс: легитимный 404-JSON от опенкода (сессия снесена) —
        // по-прежнему Dead, снифф его не задевает
        let base = stub_server(
            "HTTP/1.1 404 Not Found",
            "application/json",
            "{\"errors\":[]}",
        );
        let c = OpencodeClient::new(base, "p".into(), None);
        assert_eq!(c.liveness("ses_gone").await, Liveness::Dead);
    }

    #[tokio::test]
    async fn last_activity_ms_gate_html_stub_is_error_stall_logic_not_blind() {
        // #1160, пин тестировщика: GET /message на заглушке — сталл-логике
        // нужен Err («проба не удалась»), а не Ok(None) «активности нет»:
        // слепой сталл-детект — соседний класс «всё пусто/всё ок»
        let base = stub_server(
            "HTTP/1.1 200 OK",
            "text/html; charset=utf-8",
            GATE_STUB_BODY,
        );
        let c = OpencodeClient::new(base, "p".into(), None);
        let res = c.last_activity_ms("ses_x").await;
        assert!(
            res.is_err(),
            "HTML-заглушка не должна выглядеть как отсутствие активности: {res:?}"
        );
    }

    // ——— #1221: компакт-ручки клиента — pathname и семантика ———

    fn assistant_msg(input: u64) -> serde_json::Value {
        serde_json::json!({
            "type": "assistant",
            "tokens": {
                "input": input, "output": 100, "reasoning": 10,
                "cache": { "read": 50, "write": 5 }
            },
            "time": { "created": 1_000 }
        })
    }

    fn catalog_entry(provider: &str, id: &str, context: u64) -> serde_json::Value {
        serde_json::json!({
            "providerID": provider, "modelID": id,
            "limit": { "context": context }
        })
    }

    fn client(base: &str) -> OpencodeClient {
        OpencodeClient::new(base.to_string(), "p".into(), None)
    }

    #[tokio::test]
    async fn compact_posts_native_endpoint_pathname() {
        let (base, hits) = stub_multi(vec![("HTTP/1.1 200 OK", "{}".into())]);
        client(&base)
            .compact_session("ses1")
            .await
            .expect("compact");
        assert_eq!(
            hits.lock().expect("hits").as_slice(),
            ["POST /api/session/ses1/compact"],
            "нативный эндпоинт, одиночный /api-префикс (класс двойного — ADR-082)"
        );
    }

    #[tokio::test]
    async fn compact_http_error_is_err() {
        let (base, hits) = stub_multi(vec![("HTTP/1.1 500 Internal Server Error", "{}".into())]);
        assert!(client(&base).compact_session("ses1").await.is_err());
        assert_eq!(hits.lock().expect("hits").len(), 1);
    }

    #[tokio::test]
    async fn context_usage_pct_from_tail_and_catalog() {
        let tail = serde_json::json!([assistant_msg(900)]).to_string();
        let catalog = serde_json::json!([catalog_entry("p", "m", 2000)]).to_string();
        let (base, hits) = stub_multi(vec![
            ("HTTP/1.1 200 OK", tail),
            ("HTTP/1.1 200 OK", catalog),
        ]);
        let m = ModelRef::new("p", "m");
        let pct = client(&base)
            .context_usage_pct("s", &m)
            .await
            .expect("probe")
            .expect("measured");
        // (900+100+10+50+5)/2000 = 53.25%
        assert!((pct - 53.25).abs() < 1e-9, "pct = {pct}");
        assert_eq!(
            hits.lock().expect("hits").as_slice(),
            [
                "GET /api/session/s/message?limit=10&order=desc",
                "GET /api/model"
            ],
            "хвост затем каталог, оба с одиночным /api"
        );
    }

    #[tokio::test]
    async fn no_assistant_turn_means_unmeasured_without_catalog_call() {
        let tail = serde_json::json!([{
            "type": "user", "time": { "created": 1_000 }
        }])
        .to_string();
        let (base, hits) = stub_multi(vec![("HTTP/1.1 200 OK", tail)]);
        let m = ModelRef::new("p", "m");
        let got = client(&base)
            .context_usage_pct("s", &m)
            .await
            .expect("probe");
        assert_eq!(got, None);
        assert_eq!(hits.lock().expect("hits").len(), 1, "каталог не нужен");
    }

    #[tokio::test]
    async fn model_limit_cached_across_calls() {
        let tail = serde_json::json!([assistant_msg(900)]).to_string();
        let catalog = serde_json::json!([catalog_entry("p", "m", 2000)]).to_string();
        let (base, hits) = stub_multi(vec![
            ("HTTP/1.1 200 OK", tail.clone()),
            ("HTTP/1.1 200 OK", catalog),
            ("HTTP/1.1 200 OK", tail),
        ]);
        let m = ModelRef::new("p", "m");
        let c = client(&base);
        let first = c
            .context_usage_pct("s", &m)
            .await
            .expect("probe")
            .expect("measured");
        let second = c
            .context_usage_pct("s", &m)
            .await
            .expect("probe")
            .expect("measured");
        assert_eq!(first, second);
        let h = hits.lock().expect("hits");
        assert_eq!(
            h.iter().filter(|l| l.ends_with("/model")).count(),
            1,
            "лимит берётся из кэша: /model ровно один раз"
        );
        assert_eq!(h.len(), 3);
    }

    #[tokio::test]
    async fn model_absence_cached_forever() {
        // перманентное отсутствие модели в каталоге — Ok(None) кэшируется
        // навсегда (фиксация ревью круга 1 ADR-082): повторные вызовы
        // каталог не дёргают
        let tail = serde_json::json!([assistant_msg(900)]).to_string();
        let catalog = serde_json::json!([catalog_entry("other", "x", 1000)]).to_string();
        let (base, hits) = stub_multi(vec![
            ("HTTP/1.1 200 OK", tail.clone()),
            ("HTTP/1.1 200 OK", catalog),
            ("HTTP/1.1 200 OK", tail),
        ]);
        let m = ModelRef::new("p", "m");
        let c = client(&base);
        assert_eq!(c.context_usage_pct("s", &m).await.expect("probe"), None);
        assert_eq!(c.context_usage_pct("s", &m).await.expect("probe"), None);
        let h = hits.lock().expect("hits");
        assert_eq!(h.iter().filter(|l| l.ends_with("/model")).count(), 1);
    }

    #[tokio::test]
    async fn model_catalog_transport_error_not_cached() {
        // transient-сбой каталога — Err без кэша: следующий вызов
        // ретраит /model (фиксация ревью круга 1 ADR-082)
        let tail = serde_json::json!([assistant_msg(900)]).to_string();
        let catalog = serde_json::json!([catalog_entry("p", "m", 2000)]).to_string();
        let (base, hits) = stub_multi(vec![
            ("HTTP/1.1 200 OK", tail.clone()),
            ("HTTP/1.1 500 Internal Server Error", "{}".into()),
            ("HTTP/1.1 200 OK", tail),
            ("HTTP/1.1 200 OK", catalog),
        ]);
        let m = ModelRef::new("p", "m");
        let c = client(&base);
        assert!(
            c.context_usage_pct("s", &m).await.is_err(),
            "transient — Err"
        );
        assert!(
            c.context_usage_pct("s", &m).await.expect("probe").is_some(),
            "ретрай следующим вызовом"
        );
        let h = hits.lock().expect("hits");
        assert_eq!(h.iter().filter(|l| l.ends_with("/model")).count(), 2);
    }
}
