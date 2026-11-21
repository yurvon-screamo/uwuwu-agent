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

mod common;

use common::{probe_session, spawn_mock, wait_ready};
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

// ---------- юниты парсера и канонизации ----------
// Синтетические ветки, которых нет в живой фикстуре (реальный клиент их
// не содержит): канонизационные срезы, экранированные кавычки, оборванный
// литерал, непарный литерал без следующего Method, литерал внутри комментария.

#[test]
fn canonical_slices_query_api_and_normalizes_placeholders() {
    // срез query
    assert_eq!(canonical("/session?limit=50&order=desc"), "/session");
    // срез /api (роутерная форма манифеста)
    assert_eq!(
        canonical("/api/session/{id}/prompt"),
        "/session/{id}/prompt"
    );
    // фактическое поведение: strip_prefix("/api") жадный — «/apix» тоже
    // срезается до «x/…». Пины не ослаблены: канонизация симметрична
    // (обе стороны сравнения режутся одной функцией), путь не совпадёт
    // с манифестом → громкий красный, не тихий пропуск
    assert_eq!(canonical("/apix/session"), "x/session");
    // плейсхолдеры {…} любой длины → {id}
    assert_eq!(
        canonical("/session/{sessionId}/agent"),
        "/session/{id}/agent"
    );
    assert_eq!(canonical("/{PAGE_LIMIT}/x"), "/{id}/x");
    // комбинация всего разом
    assert_eq!(
        canonical("/api/session/{session}/message?limit=1"),
        "/session/{id}/message"
    );
    // без плейсхолдеров и без query — как есть
    assert_eq!(canonical("/session"), "/session");
}

#[test]
fn impl_part_cuts_at_first_cfg_test_marker() {
    assert_eq!(impl_part("a\"/p\"b#[cfg(test)]\"/x\""), "a\"/p\"b");
    // маркера нет — весь текст
    assert_eq!(impl_part("a\"/p\""), "a\"/p\"");
}

#[test]
fn closing_quote_skips_escaped_quotes_and_reports_unterminated() {
    // экранированная кавычка внутри литерала не закрывает его
    let src = "\"a\\\"b/\" tail";
    assert_eq!(closing_quote(src.as_bytes(), 1), Some(6));
    // оборванный литерал
    assert_eq!(closing_quote("\"abc".as_bytes(), 1), None);
}

#[test]
fn method_after_takes_nearest_method_or_none() {
    let src = "\"/session\" reqwest::Method::POST .call(\"/x\") reqwest::Method::GET";
    assert_eq!(method_after(src, 1).as_deref(), Some("POST"));
    // Method после литерала нет вообще
    assert_eq!(method_after("\"/lonely\"", 1), None);
}

#[test]
fn leading_slash_literals_finds_literals_inside_comments_too() {
    // парсер не понимает комментарии: литерал в комменте — «маршрутный»
    // кандидата; тем самым посторонний литерал в комменте рвёт подмножество
    // или канарейку громко, не уходит тихим пропуском
    let found = leading_slash_literals("// \"/commented-path\" ok");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].1, "/commented-path");
    // экранированная кавычка не рвёт разбор
    let found = leading_slash_literals("\"/a\\\"b\" x");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].1, "/a\\\"b");
}

// ---------- пин 2: проб роутера ----------

#[tokio::test]
async fn mock_router_serves_every_manifest_route() {
    let (base, _mock) = spawn_mock(&[]);
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
