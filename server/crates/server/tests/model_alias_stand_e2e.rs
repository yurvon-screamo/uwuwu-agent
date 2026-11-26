//! E2E главного сценария #1684 (гейт `UWUWU_E2E=1`): живой uwuwu-server
//! (hub driver=memory, executor = mock_opencode) получает алиасы из конфига
//! и кладёт разрешённую модель в тело каждого спавна.
//!
//! Сценарий, который тестировщик гонял руками на стенде (`scratch/alias_probe`):
//!   1. разная raw у трёх алиасов → flash-агенты и bootstrap несут flash,
//!      tester — common, ревьювер — smart;
//!   2. правка raw только у алиаса flash → вся flash-группа едет на новой
//!      модели без правок по агентам; tester и ревьювер не задеты.
//!
//! Тела POST /api/session ловит тестовый прокси из общего харнеса
//! (`capture.jsonl`): сам mock_opencode модель в лог не пишет.
//!
//! Запуск: UWUWU_E2E=1 cargo test -p uwuwu-server --test model_alias_stand_e2e

mod common;

use common::stand::{e2e_enabled, log_tail, spawn_stand, StandOptions};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

const ALIASES_V2: &str = "[executor.model_aliases]\n\
    flash = \"p/flash-v2#high\"\n\
    common = \"p/common-v2#high\"\n\
    smart = \"p/smart-v2#high\"\n\n";

const ALIASES_V3: &str = "[executor.model_aliases]\n\
    flash = \"p/flash-v3#high\"\n\
    common = \"p/common-v2#high\"\n\
    smart = \"p/smart-v2#high\"\n\n";

const ASSIGNMENTS: &str = "[executor.models]\n\
    default = \"flash\"\n\
    developer = \"flash\"\n\
    analyst = \"flash\"\n\
    tester = \"common\"\n\
    plan-reviewer = \"smart\"\n\
    code-quality-reviewer = \"smart\"\n\
    test-reviewer = \"smart\"\n\
    analyst-reviewer = \"smart\"\n\
    dreamer = \"common\"\n\n";

const SEEDS: &str = "901:Testing;902:Design;903:Analys;904:Result review;905:Plan review";
const FLASH_V2: &str = "p/flash-v2/high";
const FLASH_V3: &str = "p/flash-v3/high";
const COMMON_V2: &str = "p/common-v2/high";
const SMART_V2: &str = "p/smart-v2/high";

fn model_of(body: &Value) -> String {
    let model = &body["model"];
    format!(
        "{}/{}/{}",
        model["providerID"].as_str().unwrap_or_default(),
        model["id"].as_str().unwrap_or_default(),
        model["variant"].as_str().unwrap_or_default()
    )
}

/// Агент → модель из перехваченных тел живых спавнов. У bootstrap-сессии
/// поля `agent` нет — она числится как `default`.
fn captured_models(dir: &Path) -> HashMap<String, String> {
    let text = std::fs::read_to_string(dir.join("capture.jsonl")).unwrap_or_default();
    let mut seen = HashMap::new();
    for line in text.lines() {
        let Ok(rec) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if rec["method"].as_str() != Some("POST")
            || !rec["path"]
                .as_str()
                .unwrap_or_default()
                .ends_with("/api/session")
        {
            continue;
        }
        let agent = rec["body"]["agent"]
            .as_str()
            .unwrap_or("default")
            .to_string();
        seen.insert(agent, model_of(&rec["body"]));
    }
    seen
}

async fn wait_models(dir: &Path, agents: &[&str]) -> HashMap<String, String> {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let seen = captured_models(dir);
        if agents.iter().all(|a| seen.contains_key(*a)) {
            return seen;
        }
        assert!(
            Instant::now() < deadline,
            "спавны не дошли до всех агентов: {:?}\ncapture: {:?}\nserver: {}",
            agents,
            seen,
            log_tail(&dir.join("server.log"))
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[tokio::test]
async fn alias_layers_reach_live_spawns_and_one_edit_moves_whole_group() {
    if !e2e_enabled() {
        println!("e2e выключен: UWUWU_E2E=1 для пробы живого стенда");
        return;
    }

    let want_agents = ["default", "developer", "analyst", "tester", "plan-reviewer"];
    let config_v2 = format!("{ALIASES_V2}{ASSIGNMENTS}");
    let config_v3 = format!("{ALIASES_V3}{ASSIGNMENTS}");
    let v2_dir = std::env::temp_dir().join(format!("alias-e2e-v2-{}", std::process::id()));
    let v3_dir = std::env::temp_dir().join(format!("alias-e2e-v3-{}", std::process::id()));
    for dir in [&v2_dir, &v3_dir] {
        let _ = std::fs::remove_dir_all(dir);
    }

    // 1. Раскладка опроса доезжает до живых спавнов.
    let stand_v2 = spawn_stand(
        &v2_dir,
        StandOptions {
            config_extra: &config_v2,
            pre_state: None,
            server_env: &[("UWUWU_MOCK_SEED", SEEDS)],
            capture_sessions: true,
        },
    )
    .await;
    let v2 = wait_models(&v2_dir, &want_agents).await;
    for agent in ["default", "developer", "analyst"] {
        assert_eq!(
            v2.get(agent).map(String::as_str),
            Some(FLASH_V2),
            "{agent}: flash-группа не получила raw алиса flash"
        );
    }
    assert_eq!(
        v2.get("tester").map(String::as_str),
        Some(COMMON_V2),
        "tester — common"
    );
    assert_eq!(
        v2.get("plan-reviewer").map(String::as_str),
        Some(SMART_V2),
        "ревьювер — smart"
    );
    drop(stand_v2);

    // 2. Одна правка raw у алиса flash: group едет на новой модели,
    //    назначения агентов и остальные алиасы не тронуты.
    let stand_v3 = spawn_stand(
        &v3_dir,
        StandOptions {
            config_extra: &config_v3,
            pre_state: None,
            server_env: &[("UWUWU_MOCK_SEED", SEEDS)],
            capture_sessions: true,
        },
    )
    .await;
    let v3 = wait_models(&v3_dir, &want_agents).await;
    for agent in ["default", "developer", "analyst"] {
        assert_eq!(
            v3.get(agent).map(String::as_str),
            Some(FLASH_V3),
            "{agent}: правка алиса flash не доехала до группы"
        );
    }
    assert_eq!(
        v3.get("tester").map(String::as_str),
        Some(COMMON_V2),
        "tester не должен измениться от правки flash"
    );
    assert_eq!(
        v3.get("plan-reviewer").map(String::as_str),
        Some(SMART_V2),
        "ревьювер не должен измениться от правки flash"
    );
    drop(stand_v3);

    let _ = std::fs::remove_dir_all(&v2_dir);
    let _ = std::fs::remove_dir_all(&v3_dir);
}
