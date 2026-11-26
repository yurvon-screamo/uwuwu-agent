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
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Каталог стенда убирается и при красном прогоне (мутации/паника) —
/// четыре прогона не оставляют хвост в системном temp.
struct AliasTempDirGuard(PathBuf);

impl AliasTempDirGuard {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!("alias-e2e-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        Self(path)
    }
}

impl Drop for AliasTempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

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
    code-quality-reviewer = \"flash\"\n\
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

    let want_agents = [
        "default",
        "developer",
        "analyst",
        "tester",
        "plan-reviewer",
        "code-quality-reviewer",
    ];
    let config_v2 = format!("{ALIASES_V2}{ASSIGNMENTS}");
    let config_v3 = format!("{ALIASES_V3}{ASSIGNMENTS}");
    let dir_v2 = AliasTempDirGuard::new("v2");
    let dir_v3 = AliasTempDirGuard::new("v3");

    // 1. Раскладка опроса доезжает до живых спавнов.
    let stand_v2 = spawn_stand(
        &dir_v2.0,
        StandOptions {
            config_extra: &config_v2,
            pre_state: None,
            server_env: &[("UWUWU_MOCK_SEED", SEEDS)],
            capture_sessions: true,
        },
    )
    .await;
    let v2 = wait_models(&dir_v2.0, &want_agents).await;
    for agent in ["default", "developer", "analyst"] {
        assert_eq!(
            v2.get(agent).map(String::as_str),
            Some(FLASH_V2),
            "{agent}: flash-группа не получила raw алиаса flash"
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
    // Назначение неканоническое: код-ревьювер переназначен на flash.
    // Именно этот слой доезжает до живого спавна и проверяется здесь;
    // мутация клея, отдающая назначения как None, краснит этот ассерт.
    assert_eq!(
        v2.get("code-quality-reviewer").map(String::as_str),
        Some(FLASH_V2),
        "назначение code-quality-reviewer → flash не доехало до спавна"
    );
    drop(stand_v2);

    // 2. Одна правка raw у алиаса flash: группа едет на новой модели,
    //    назначения агентов и остальные алиасы не тронуты.
    let stand_v3 = spawn_stand(
        &dir_v3.0,
        StandOptions {
            config_extra: &config_v3,
            pre_state: None,
            server_env: &[("UWUWU_MOCK_SEED", SEEDS)],
            capture_sessions: true,
        },
    )
    .await;
    let v3 = wait_models(&dir_v3.0, &want_agents).await;
    for agent in ["default", "developer", "analyst"] {
        assert_eq!(
            v3.get(agent).map(String::as_str),
            Some(FLASH_V3),
            "{agent}: правка алиаса flash не доехала до группы"
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
        "ревьювер на smart не должен измениться от правки flash"
    );
    assert_eq!(
        v3.get("code-quality-reviewer").map(String::as_str),
        Some(FLASH_V3),
        "ревьювер, переназначенный на flash, едет вместе с группой"
    );
    drop(stand_v3);
}
