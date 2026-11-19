//! Route-coverage пин-тест дрейфа мока (#1329): новый вызов сервер →
//! opencode без мок-роута — красный сигнал до живого прогона стенда
//! (класс инстанса #1311: `POST /session/:id/agent` вёл стенд в тихий
//! 404-стояк, ад-хок фикс 49a080d1).
//!
//! Цепочка правды: вызовы клиента ⊆ MOCK_ROUTES ⊆ роутер мока.
//!   пин 1 (скан): path-литералы executor/opencode.rs ⊆ манифест
//!   пин 2 (проб): каждый маршрут манифеста роутер реально обслуживает
//!
//! Носитель красного сигнала — локальный self-check (cargo test) и
//! раунд тестировщика: авто-триггеры Actions сняты 17.11 по биллингу
//! (72c40fb3); при возврате PR-триггеров тест — PR-гейт без правок.

#[path = "../src/mock_routes.rs"]
mod mock_routes;

use mock_routes::{canonical, MOCK_ROUTES};

// ---------- пин 1: скан клиента ----------

/// Исходник клиента. include_str! мёртвым путём — compile-error, не тишина.
const CLIENT_SRC: &str = include_str!("../src/executor/opencode.rs");

/// Impl-часть: до первого `#[cfg(test)]` — литералы тест-модуля
/// (`"/tmp"`, `"/x"`) не маршруты.
fn impl_part(src: &str) -> &str {
    match src.find("#[cfg(test)]") {
        Some(pos) => &src[..pos],
        None => src,
    }
}

/// Конец строкового литерала (индекс закрывающей кавычки) от `start`
/// (первого байта после открывающей); None — литерал оборван.
/// Экранированные кавычки пропускаются; raw-строки в файле не водятся —
/// при появлении разбор сломается громко (красный тест), не тихо.
fn closing_quote(bytes: &[u8], start: usize) -> Option<usize> {
    let mut j = start;
    while j < bytes.len() {
        match bytes[j] {
            b'"' => return Some(j),
            b'\\' => j += 1,
            _ => {}
        }
        j += 1;
    }
    None
}

/// Строковые литералы с ведущим '/' — ручной разбор (regex запрещён).
fn leading_slash_literals(src: &str) -> Vec<(usize, &str)> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'"' {
            i += 1;
            continue;
        }
        let Some(end) = closing_quote(bytes, i + 1) else {
            break;
        };
        let lit = &src[i + 1..end];
        if lit.starts_with('/') {
            out.push((i, lit));
        }
        i = end + 1;
    }
    out
}

/// Метод литерала: следующий `reqwest::Method::X` после него (конвенция
/// call(path, method, ...) — путь всегда впереди метода).
fn method_after(src: &str, from: usize) -> Option<String> {
    const NEEDLE: &str = "reqwest::Method::";
    let pos = src[from..].find(NEEDLE)? + from;
    let rest = &src[pos + NEEDLE.len()..];
    let name: String = rest
        .chars()
        .take_while(|c| c.is_ascii_uppercase())
        .collect();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

#[test]
fn client_route_literals_are_covered_by_mock_manifest() {
    let impl_src = impl_part(CLIENT_SRC);
    let client_routes: Vec<(String, String)> = leading_slash_literals(impl_src)
        .into_iter()
        .map(|(pos, lit)| {
            let method = method_after(impl_src, pos + lit.len()).unwrap_or_else(|| {
                panic!("литерал «{lit}» без следующего reqwest::Method — не маршрут? поправь вызов или область скана");
            });
            (method, canonical(lit))
        })
        .collect();
    let manifest: std::collections::HashSet<_> = MOCK_ROUTES
        .iter()
        .map(|(m, p)| (m.to_string(), canonical(p)))
        .collect();
    // сначала подмножество — фейл несёт имя маршрута
    let missing: Vec<_> = client_routes
        .iter()
        .filter(|r| !manifest.contains(r))
        .collect();
    assert!(
        missing.is_empty(),
        "клиент зовёт маршруты, которых нет в роутере mock_opencode: {missing:?} — \
         дополни MOCK_ROUTES и роутер мока по контракту opencode"
    );
    // канарейка: маршрутных литералов ровно столько, сколько call-сайтов
    // во всём файле (call-сайт с путём вне одного литерала — тоже красный)
    let calls = CLIENT_SRC.matches(".call(").count();
    assert_eq!(
        client_routes.len(),
        calls,
        "канарейка: маршрутных литералов {} vs .call( сайтов {calls} — \
         у вызова нет литерала-пути или литерал посторонний: {client_routes:?}",
        client_routes.len()
    );
}

// ---------- пин 2: проб роутера ----------

/// Спавн бинаря мока на свободном порту; Drop-гард убивает процесс.
struct MockProc(std::process::Child);

impl Drop for MockProc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_mock() -> (String, MockProc) {
    use std::process::{Command, Stdio};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("free port");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);

    let child = Command::new(env!("CARGO_BIN_EXE_mock_opencode"))
        .env("MOCK_MODE", "plain")
        .env("MOCK_PORT", port.to_string())
        .env("MOCK_STEP_MS", "50")
        .env("MOCK_MCP_URL", "http://127.0.0.1:1")
        .env("MOCK_MCP_TOKEN", "")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn mock_opencode (cargo строит бин для integration-тестов)");
    (format!("http://127.0.0.1:{port}"), MockProc(child))
}

async fn wait_ready(base: &str) {
    let http = reqwest::Client::new();
    for _ in 0..100 {
        if let Ok(resp) = http.get(format!("{base}/api/session")).send().await {
            if resp.status().is_success() {
                return;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("мок не поднялся за 5с: {base}");
}

/// Свежая probe-сессия: маршруты с {id} ходят каждый по своей — DELETE
/// не ломает соседей.
async fn probe_session(base: &str, http: &reqwest::Client) -> String {
    let resp = http
        .post(format!("{base}/api/session"))
        .json(&serde_json::json!({ "title": "route probe" }))
        .send()
        .await
        .expect("create probe session");
    let body: serde_json::Value = resp.json().await.expect("create json");
    body["data"]["id"].as_str().expect("probe id").to_string()
}

#[tokio::test]
async fn mock_router_serves_every_manifest_route() {
    let (base, _mock) = spawn_mock();
    wait_ready(&base).await;
    let http = reqwest::Client::new();

    let mut unrouted = Vec::new();
    for (method, path) in MOCK_ROUTES {
        let id = if path.contains("{id}") {
            Some(probe_session(&base, &http).await)
        } else {
            None
        };
        let real = match &id {
            Some(id) => path.replace("{id}", id),
            None => path.to_string(),
        };
        let req = match *method {
            "GET" => http.get(format!("{base}{real}")),
            "POST" => http
                .post(format!("{base}{real}"))
                // Json-экстракторы мока читают поля с unwrap_or_default —
                // пустого тела достаточно
                .json(&serde_json::json!({})),
            "DELETE" => http.delete(format!("{base}{real}")),
            other => panic!("манифест несёт незнакомый метод {other} — дополни проб-тест"),
        };
        let status = req.send().await.expect("probe request").status();
        // 405 = путь маршрутизируется, но метод разошёлся — тоже дрейф
        if status.as_u16() == 404 || status.as_u16() == 405 {
            unrouted.push(format!("{method} {path} → {status}"));
        }
    }
    assert!(
        unrouted.is_empty(),
        "роутер mock_opencode не обслуживает маршруты манифеста (или метод разошёлся): \
         {unrouted:?} — синхронизируй роутер с MOCK_ROUTES"
    );
}
