//! E2E-пин сессионного контракта досок (ADR-054/ADR-083, задача
//! 20261110081338-lfrrsps): на всех пяти фабричных досках существуют колонки
//! «Сессия dev» / «Сессия ревью» / «Сессия тест» (text), во всех работающих
//! views (table/kanban/list) стоят подряд dev → ревью → тест и видимы
//! (hidden=false). До задачи 10.11 колонок ревью/тест не было — записи фабрики
//! уходили в true no-op; удаление/скрытие колонки после задачи ломает контракт
//! привязок review/test-спавнов — этот тест обязан упасть.
//!
//! Запуск: UWUWU_E2E=1 cargo test --test boards_session_cols_e2e
//! (без живого ядра тесты сразу зелёные). Ядро — через штатный клиент
//! `SiYuan::from_env` + `post_probe` (те же env-правила, что у CLI).

use uwuwu_cli::siyuan::SiYuan;

const DESC: &str = "sessionID сессии агента: фабрика при спавне или task claim";

/// (проект, av-ид доски «Задачи <proj>»)
const BOARDS: [(&str, &str); 5] = [
    ("uwuwu", "20261104000238-96ch3l2"),
    ("origa", "20261104000043-3ujtnp0"),
    ("1xgames", "20261104093449-q2h9ju2"),
    ("true-washing", "20261104000514-69etncy"),
    ("foilcap", "20261104093225-7zgx3n8"),
];

const SESSION_COLS: [&str; 3] = ["Сессия dev", "Сессия ревью", "Сессия тест"];

/// Контейнеры колонок по типу view (keyValues-порядок av ≠ порядок view).
fn view_cols(view: &serde_json::Value) -> Vec<(String, bool)> {
    let kind = view["type"].as_str().unwrap_or_default().to_string();
    let container = match kind.as_str() {
        "kanban" => "fields",
        "gallery" => "cardFields",
        _ => "columns", // table и list: columns
    };
    let arr = &view[&kind][container];
    let Some(arr) = arr.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .map(|c| {
            (
                c["id"].as_str().unwrap_or_default().to_string(),
                c["hidden"].as_bool().unwrap_or(false),
            )
        })
        .collect()
}

fn e2e_enabled() -> bool {
    std::env::var("UWUWU_E2E").as_deref() == Ok("1")
}

/// Сессионные ключи доски: (name, id, type, desc) в порядке keyValues av.
async fn session_keys(sy: &SiYuan, proj: &str, av: &str) -> Vec<(String, String, String, String)> {
    let resp = sy
        .post_probe("/api/av/getAttributeView", serde_json::json!({ "id": av }))
        .await
        .unwrap_or_else(|e| panic!("getAttributeView {proj}: {e}"));
    let key_values = resp["data"]["av"]["keyValues"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    key_values
        .iter()
        .filter(|kv| SESSION_COLS.contains(&kv["key"]["name"].as_str().unwrap_or_default()))
        .map(|kv| {
            (
                kv["key"]["name"].as_str().unwrap_or_default().to_string(),
                kv["key"]["id"].as_str().unwrap_or_default().to_string(),
                kv["key"]["type"].as_str().unwrap_or_default().to_string(),
                kv["key"]["desc"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// Проверки одного view: подряд dev → ревью → тест, hidden=false;
/// gallery-класс (сессий нет вовсе, вкл. pre-existing dev) — задекларированное
/// исключение (display-подмножество карточек, kernel-механика).
fn check_view(
    proj: &str,
    view: &serde_json::Value,
    ids: (&Option<String>, &Option<String>, &Option<String>),
    failures: &mut Vec<String>,
) {
    let vname = view["name"].as_str().unwrap_or_default();
    let cols = view_cols(view);
    if cols.is_empty() {
        return;
    }
    let pos = |want: &Option<String>| {
        want.as_deref()
            .and_then(|id| cols.iter().position(|(cid, _)| cid == id))
    };
    let (d, r, t) = (pos(ids.0), pos(ids.1), pos(ids.2));
    match (d, r, t) {
        (None, None, None) => {}
        (Some(d), Some(r), Some(t)) => {
            if r != d + 1 || t != r + 1 {
                failures.push(format!(
                    "[{proj}/{vname}] порядок: dev@{d:?} ревью@{r:?} тест@{t:?}, ждали подряд"
                ));
            }
            for (name, idx) in [("Сессия dev", d), ("Сессия ревью", r), ("Сессия тест", t)]
            {
                if cols[idx].1 {
                    failures.push(format!("[{proj}/{vname}] {name} скрыт (hidden=true)"));
                }
            }
        }
        _ => failures.push(format!(
            "[{proj}/{vname}] частичное присутствие сессий: dev@{d:?} ревью@{r:?} тест@{t:?}"
        )),
    }
}

/// Проверки одной доски: 3/3 ключей text+desc, позиция+видимость во всех views.
async fn check_board(sy: &SiYuan, proj: &str, av: &str, failures: &mut Vec<String>) {
    let sess = session_keys(sy, proj, av).await;
    let missing: Vec<&str> = SESSION_COLS
        .iter()
        .filter(|n| !sess.iter().any(|(name, ..)| name == *n))
        .copied()
        .collect();
    if !missing.is_empty() {
        failures.push(format!("[{proj}] нет колонок: {missing:?}"));
        return;
    }
    for (name, _, kind, desc) in &sess {
        if kind != "text" {
            failures.push(format!("[{proj}] {name}: type={kind}, ждали text"));
        }
        if desc != DESC {
            failures.push(format!("[{proj}] {name}: desc={desc:?}, ждали {DESC:?}"));
        }
    }
    let id_of = |want: &str| {
        sess.iter()
            .find(|(name, ..)| name == want)
            .map(|(_, id, ..)| id.clone())
    };
    let ids = (
        id_of("Сессия dev"),
        id_of("Сессия ревью"),
        id_of("Сессия тест"),
    );

    let resp = sy
        .post_probe("/api/av/getAttributeView", serde_json::json!({ "id": av }))
        .await
        .unwrap_or_else(|e| panic!("getAttributeView {proj}: {e}"));
    let views = resp["data"]["av"]["views"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for view in &views {
        check_view(proj, view, (&ids.0, &ids.1, &ids.2), failures);
    }
}

/// Контракт сессионных колонок на пяти фабричных досках: 3/3 text+desc,
/// подряд и видимы во всех views, где сессии присутствуют.
#[tokio::test]
async fn session_columns_on_factory_boards() {
    if !e2e_enabled() {
        return;
    }
    let sy = SiYuan::from_env().expect("SiYuan::from_env (живое ядро)");
    let mut failures: Vec<String> = Vec::new();

    for (proj, av) in BOARDS {
        check_board(&sy, proj, av, &mut failures).await;
    }

    assert!(
        failures.is_empty(),
        "сессионный контракт досок нарушен:\n{}",
        failures.join("\n")
    );
}
