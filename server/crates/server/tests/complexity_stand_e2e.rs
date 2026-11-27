//! Живой e2e сценария #1689 (гейт `UWUWU_E2E=1`): реальный uwuwu-server
//! на memory-хабе и mock_opencode принимает сложность через MCP-тулы и
//! кладёт сдвинутый алиас в тело спавна.
//!
//! Сценарий тестировщика (isolated stand, 27.11):
//!   1. `task_create` со сложностью high → `task_get` несёт `complexity: high`;
//!      перевод в In Progress → тик спавнит девелопера не на базовом flash,
//!      а на common (+1); в `spawn.done` — shift=1, alias=common;
//!   2. `task_create` со сложностью low → перевод в Result review → тик
//!      спавнит код-ревьювера не на smart, а на common (−1); в `spawn.done`
//!      — shift=-1, alias=common;
//!   3. неканоническое значение отклоняется тулом до мутаций носителя.
//!
//! Тела POST /api/session ловит тестовый прокси общего харнеса
//! (`capture.jsonl`): сам mock_opencode модель в лог не пишет.
//!
//! Запуск: UWUWU_E2E=1 cargo test -p uwuwu-server --test complexity_stand_e2e
//! CI: rust-джоба `cargo test` подхватывает файл; без UWUWU_E2E тест честно
//! пропускается — тот же контур, что у остальных stand_e2e-файлов репо.

mod common;

use common::stand::{e2e_enabled, log_tail, mcp_call, spawn_stand, BEARER};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

struct ComplexityTempDirGuard(PathBuf);

impl ComplexityTempDirGuard {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "complexity-e2e-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_dir_all(&path);
        Self(path)
    }
}

impl Drop for ComplexityTempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Явная карта: три разных raw, чтобы сдвиг был виден как смена уровня алиаса.
const CONFIG_EXTRA: &str = "[executor.model_aliases]\n\
    flash = \"p/flash#high\"\n\
    common = \"p/common#high\"\n\
    smart = \"zai-coding-plan/glm-smart#high\"\n\n\
    [executor.models]\n\
    default = \"flash\"\n\
    developer = \"flash\"\n\
    plan-reviewer = \"smart\"\n\
    code-quality-reviewer = \"smart\"\n\
    tester = \"flash\"\n\
    test-reviewer = \"smart\"\n\
    analyst = \"flash\"\n\
    analyst-reviewer = \"smart\"\n\
    dreamer = \"common\"\n\n";

const FLASH: &str = "p/flash/high";
const COMMON: &str = "p/common/high";
const SMART: &str = "zai-coding-plan/glm-smart/high";
/// Поле `model` в событии — метка провайдер/модель без вариант-хвоста.
const FLASH_LOG: &str = "p/flash";
const COMMON_LOG: &str = "p/common";
const SMART_LOG: &str = "zai-coding-plan/glm-smart";

/// Агент → модель из перехваченных живых POST /api/session:raw-метка
/// provider/id/variant, чтобы сдвиг был виден до среза вариант-хвоста.
fn captured_models(dir: &Path) -> HashMap<String, String> {
    let text = std::fs::read_to_string(dir.join("capture.jsonl")).unwrap_or_default();
    let mut seen = HashMap::new();
    for rec in text
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
    {
        let is_spawn = rec["method"] == "POST"
            && rec["path"]
                .as_str()
                .is_some_and(|path| path.ends_with("/api/session"));
        if !is_spawn {
            continue;
        }
        let body = &rec["body"];
        let model = ["providerID", "id", "variant"]
            .iter()
            .map(|key| body["model"][*key].as_str().unwrap_or_default())
            .collect::<Vec<_>>()
            .join("/");
        seen.insert(
            body["agent"].as_str().unwrap_or("default").to_string(),
            model,
        );
    }
    seen
}

/// Поля наблюдаемости из события `spawn.done` — те самые, по которым
/// читается постмержная проба #1695.
fn spawn_fields(dir: &Path, number: u64) -> Option<Value> {
    let log = std::fs::read_to_string(dir.join("server.log")).unwrap_or_default();
    for line in log.lines() {
        let Ok(rec) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if rec["fields"]["event"].as_str() == Some("spawn.done")
            && rec["fields"]["number"].as_u64() == Some(number)
        {
            let f = &rec["fields"];
            return Some(json!({
                "complexity": f["complexity"],
                "shift": f["shift"],
                "alias": f["alias"],
                "model": f["model"],
            }));
        }
    }
    None
}

fn receipt_number(text: &str) -> u64 {
    text.split('#')
        .nth(1)
        .expect("номер в квитанции create")
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .unwrap()
        .parse()
        .expect("цифровой номер")
}

#[tokio::test]
async fn complexity_shifts_live_stage_spawns_and_survives_roundtrip() {
    if !e2e_enabled() {
        println!("e2e выключен: UWUWU_E2E=1 для пробы живого стенда");
        return;
    }

    let dir = ComplexityTempDirGuard::new();
    let stand = spawn_stand(
        &dir.0,
        common::stand::StandOptions {
            config_extra: CONFIG_EXTRA,
            pre_state: None,
            server_env: &[],
            capture_sessions: true,
        },
    )
    .await;
    let http = reqwest::Client::new();

    // 1. high: девелопер получает +1 уровень (flash → common).
    let created = mcp_call(
        &http,
        stand.port,
        BEARER,
        "task_create",
        json!({
            "project": "stand", "title": "e2e high complexity",
            "body": "## Постановка\n\nСдвиг +1.",
            "complexity": "high", "priority": "medium", "automerge": false
        }),
    )
    .await;
    assert!(created.contains("created"), "create high: {created}");
    let high = receipt_number(&created);
    let card = mcp_call(
        &http,
        stand.port,
        BEARER,
        "task_get",
        json!({ "project": "stand", "number": high }),
    )
    .await;
    assert!(card.contains("complexity: high"), "карта high: {card}");
    let moved = mcp_call(
        &http,
        stand.port,
        BEARER,
        "task_edit",
        json!({ "project": "stand", "number": high, "status": "In Progress" }),
    )
    .await;
    assert!(
        moved.contains("updated"),
        "перевод high в In Progress: {moved}"
    );

    // 2. low: код-ревьювер получает −1 уровень (smart → common).
    let created = mcp_call(
        &http,
        stand.port,
        BEARER,
        "task_create",
        json!({
            "project": "stand", "title": "e2e low complexity",
            "body": "## Постановка\n\nСдвиг -1.",
            "complexity": "low", "priority": "medium", "automerge": false
        }),
    )
    .await;
    assert!(created.contains("created"), "create low: {created}");
    let low = receipt_number(&created);
    let card = mcp_call(
        &http,
        stand.port,
        BEARER,
        "task_get",
        json!({ "project": "stand", "number": low }),
    )
    .await;
    assert!(card.contains("complexity: low"), "карта low: {card}");
    for status in ["In Progress", "Result review"] {
        let moved = mcp_call(
            &http,
            stand.port,
            BEARER,
            "task_edit",
            json!({ "project": "stand", "number": low, "status": status }),
        )
        .await;
        assert!(moved.contains("updated"), "перевод low в {status}: {moved}");
    }

    // 3. неканон отклоняется тулом; носитель не мутировал.
    let rejected = mcp_call(
        &http,
        stand.port,
        BEARER,
        "task_edit",
        json!({ "project": "stand", "number": high, "complexity": "мусор" }),
    )
    .await;
    assert!(
        rejected.contains("regular|high|low"),
        "неканон отклонён с перечнем: {rejected}"
    );
    let card = mcp_call(
        &http,
        stand.port,
        BEARER,
        "task_get",
        json!({ "project": "stand", "number": high }),
    )
    .await;
    assert!(
        card.contains("complexity: high"),
        "отказ не мутировал сложность: {card}"
    );

    // Ждём оба спавна и их структурированные поля.
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let captured = captured_models(&dir.0);
        if captured.contains_key("developer")
            && captured.contains_key("code-quality-reviewer")
            && spawn_fields(&dir.0, high).is_some()
            && spawn_fields(&dir.0, low).is_some()
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "спавны/поля не дошли: capture={captured:?}\nserver: {}",
            log_tail(&dir.0.join("server.log"))
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    let captured = captured_models(&dir.0);
    assert_eq!(
        captured.get("developer").map(String::as_str),
        Some(COMMON),
        "high: девелопер едет на common, базовый flash не просочился"
    );
    assert_eq!(
        captured.get("code-quality-reviewer").map(String::as_str),
        Some(COMMON),
        "low: ревьювер едет на common, базовый smart не просочился"
    );
    assert_ne!(
        captured.get("developer").map(String::as_str),
        Some(FLASH),
        "сдвиг +1 не сработал"
    );
    assert_ne!(
        captured.get("code-quality-reviewer").map(String::as_str),
        Some(SMART),
        "сдвиг -1 не сработал"
    );

    let high_fields = spawn_fields(&dir.0, high).expect("поля high-спавна");
    assert_eq!(high_fields["complexity"], "high");
    assert_eq!(high_fields["shift"], 1);
    assert_eq!(high_fields["alias"], "common");
    assert_ne!(high_fields["model"], FLASH_LOG, "сдвиг +1 не виден в логе");
    assert_eq!(high_fields["model"], COMMON_LOG);
    let low_fields = spawn_fields(&dir.0, low).expect("поля low-спавна");
    assert_eq!(low_fields["complexity"], "low");
    assert_eq!(low_fields["shift"], -1);
    assert_eq!(low_fields["alias"], "common");
    assert_ne!(low_fields["model"], SMART_LOG, "сдвиг -1 не виден в логе");
    assert_eq!(low_fields["model"], COMMON_LOG);
}
