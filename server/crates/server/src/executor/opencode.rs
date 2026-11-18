//! opencode-исполнитель: REST-клиент, реализация AgentSessionGateway.
//! Endpoints сверен с openapi опенкода 2.3.0.

use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::Duration;
use uwuwu_usecase::{AgentSessionGateway, AssistantReply, Liveness, SessionInfo, SessionSpec};
use uwuwu_usecase::{UsecaseError, UsecaseResult};

pub struct OpencodeClient {
    base: String,
    password: String,
    http: reqwest::Client,
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

/// Тело ответа message-эндпоинта opencode: голый массив ИЛИ обёртка
/// {data: [...]} (обе формы в живом проде, opencode 2.3 — инцидент
/// «строки стоят, сессии спят» 17.11: last_activity_ms не знал обёртки,
/// вечное Ok(None) выключало сталл-детект целиком).
fn message_array(resp: &Value) -> Option<&Vec<Value>> {
    resp.as_array().or_else(|| resp["data"].as_array())
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
                    Some(SessionInfo {
                        id: s["id"].as_str()?.to_string(),
                        agent: s["agent"].as_str().unwrap_or_default().to_string(),
                        directory: s["location"]["directory"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        updated_ms: s["time"]["updated"].as_u64().unwrap_or_default(),
                        outcome: s["outcome"].as_str().unwrap_or_default().to_string(),
                        total_tokens: s["tokens"]["input"].as_u64().unwrap_or_default()
                            + s["tokens"]["output"].as_u64().unwrap_or_default(),
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
/// путь в страницу «Войти», #1160): content-type text/html ИЛИ
/// doctype-снифф тела. Детектор вызывается в call() — единственной точке
/// парсинга — поэтому накрывает всех потребителей трейта; HTML-ответ =
/// отказ пробы (Err), не «факт» о сессиях.
fn html_gate_response(content_type: Option<&str>, body: &str) -> bool {
    if content_type.is_some_and(|ct| ct.to_ascii_lowercase().contains("text/html")) {
        return true;
    }
    let lower = body.trim_start().to_ascii_lowercase();
    lower.starts_with("<!doctype html") || lower.starts_with("<html")
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
        }
    }

    async fn call(
        &self,
        path: &str,
        method: reqwest::Method,
        body: Option<Value>,
    ) -> UsecaseResult<(reqwest::StatusCode, Value)> {
        let mut req = self
            .http
            .request(method, format!("{}/api{path}", self.base))
            .basic_auth("opencode", Some(&self.password));
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
}

#[async_trait]
impl AgentSessionGateway for OpencodeClient {
    async fn create_session(&self, spec: &SessionSpec) -> UsecaseResult<String> {
        let mut body = json!({
            "title": spec.title.chars().take(64).collect::<String>(),
            "location": { "directory": spec.directory },
        });
        if let Some(name) = &spec.custom_agent {
            // именованный агент опенкода (agents/<name>.md в cwd):
            // боты с fm agent — методичка вшита системой
            body["agent"] = json!(name);
        } else {
            match &spec.agent {
                uwuwu_domain::AgentSlot::Role(agent) => {
                    body["agent"] = json!(uwuwu_usecase::agent_name(*agent));
                }
                uwuwu_domain::AgentSlot::Default => {}
            }
        }
        if let Some(model) = &spec.model {
            let mut m = json!({ "providerID": model.provider(), "id": model.id() });
            if let Some(v) = &spec.variant {
                m["variant"] = json!(v);
            }
            body["model"] = m;
        }
        let (status, resp) = self
            .call("/session", reqwest::Method::POST, Some(body))
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
                &format!("/session/{session}/prompt"),
                reqwest::Method::POST,
                Some(json!({ "text": text })),
            )
            .await?;
        Self::check(status, &format!("prompt {session}"))
    }

    async fn run_command(&self, session: &str, name: &str, args: &str) -> UsecaseResult<()> {
        let (status, _) = self
            .call(
                &format!("/session/{session}/command"),
                reqwest::Method::POST,
                Some(json!({ "name": name, "text": args })),
            )
            .await?;
        Self::check(status, &format!("command {session}/{name}"))
    }

    async fn liveness(&self, session: &str) -> Liveness {
        match self
            .call(&format!("/session/{session}"), reqwest::Method::GET, None)
            .await
        {
            Ok((status, _)) => liveness_of_status(status),
            Err(_) => Liveness::Unknown,
        }
    }

    async fn delete_session(&self, session: &str) -> UsecaseResult<()> {
        let (status, _) = self
            .call(
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
            let (status, resp) = self.call(&url, reqwest::Method::GET, None).await?;
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
