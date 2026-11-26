//! Кросс-гвард канона моделей (#1154, #1684): canonical() ↔ два раздела
//! config.example.toml И живого config.toml совпадают verbatim. Dockerfile
//! копирует именно живой config.toml, поэтому проверяются оба файла.

use std::collections::HashMap;
use uwuwu_usecase::{ModelAlias, SpawnModels};

/// Раздел под `[executor]` из TOML-файла (пример или живой конфиг).
fn table_of(path: &std::path::Path, section: &str) -> HashMap<String, String> {
    let raw =
        std::fs::read_to_string(path).unwrap_or_else(|_| panic!("{} читается", path.display()));
    let value: toml::Value = raw.parse().expect("валидный toml");
    let table = value["executor"][section]
        .as_table()
        .unwrap_or_else(|| panic!("секция [executor.{section}] присутствует"));
    table
        .iter()
        .map(|(key, model)| {
            (
                key.clone(),
                model
                    .as_str()
                    .expect("значение раздела — строка")
                    .to_string(),
            )
        })
        .collect()
}

/// Сверка обоих слоёв одного файла с каноном: ключи, алиасы, назначения
/// и суммарный for_agent-проход.
fn assert_matches_canonical(path: &std::path::Path, origin: &str) {
    let alias_raw = table_of(path, "model_aliases");
    let assignment_raw = table_of(path, "models");
    let canonical = SpawnModels::canonical();
    let configured = SpawnModels::from_config(Some(&alias_raw), Some(&assignment_raw))
        .unwrap_or_else(|error| panic!("{origin}: карта моделей не собралась: {error}"));

    // ключи раздела алиасов совпадают с лестницей — rogue-ключ ловится здесь
    let mut alias_keys: Vec<&str> = alias_raw.keys().map(String::as_str).collect();
    alias_keys.sort_unstable();
    let mut canon_alias_keys: Vec<&str> = ModelAlias::ALL.iter().map(|a| a.as_str()).collect();
    canon_alias_keys.sort_unstable();
    assert_eq!(
        alias_keys, canon_alias_keys,
        "{origin} [executor.model_aliases] ≠ canonical: ключи разошлись"
    );
    for alias in ModelAlias::ALL {
        assert_eq!(
            configured.model_for_alias(alias),
            canonical.model_for_alias(alias),
            "{origin} ↔ canonical: алиас {} разошёлся",
            alias.as_str()
        );
    }

    // ключи назначений и само назначение сверяются напрямую — фоллбэк
    // for_agent маскировал бы рассинхрон
    assert_eq!(
        configured.keys(),
        canonical.keys(),
        "{origin} [executor.models] ≠ canonical: ключи разошлись"
    );
    for key in canonical.keys() {
        assert_eq!(
            configured.alias_for_key(key),
            canonical.alias_for_key(key),
            "{origin} ↔ canonical: назначение '{key}' разошлось"
        );
        assert_eq!(
            configured.get(key),
            canonical.get(key),
            "{origin} ↔ canonical: модель '{key}' разошлась"
        );
    }

    // суммарный контракт конвейера
    for agent in [
        uwuwu_domain::Agent::Developer,
        uwuwu_domain::Agent::PlanReviewer,
        uwuwu_domain::Agent::CodeQualityReviewer,
        uwuwu_domain::Agent::Tester,
        uwuwu_domain::Agent::TestReviewer,
        uwuwu_domain::Agent::Analyst,
        uwuwu_domain::Agent::AnalystReviewer,
        uwuwu_domain::Agent::Dreamer,
    ] {
        assert_eq!(
            configured.for_agent(agent),
            canonical.for_agent(agent),
            "{origin} ↔ canonical: {agent:?} разошлись"
        );
    }
    assert_eq!(
        configured.for_default(),
        canonical.for_default(),
        "{origin} ↔ canonical: default разошлись"
    );
}

#[test]
fn canonical_matches_config_example_verbatim() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config.example.toml");
    assert_matches_canonical(&path, "config.example");
}

#[test]
fn canonical_matches_live_config_verbatim() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config.toml");
    assert_matches_canonical(&path, "config.toml");
}

/// Главный сценарий владельца (US-1/UC-1): raw алиаса flash задан один раз,
/// а назначение tester — неканоническое — доезжает через ту же границу.
/// Inline TOML → ExecutorCfg → from_config → плоская карта под тестом.
#[test]
fn alias_raw_from_toml_drives_whole_group_and_noncanonical_assignment() {
    let raw = r#"
        name = "kisa"
        url = "https://example.invalid"

        [model_aliases]
        flash = "p/new-flash#high"
        common = "p/common#high"
        smart = "p/smart#high"

        [models]
        default = "flash"
        developer = "flash"
        analyst = "flash"
        tester = "flash"
        plan-reviewer = "smart"
    "#;
    let cfg: crate::config::ExecutorCfg = toml::from_str(raw).expect("валидный toml");
    let models = SpawnModels::from_config(cfg.model_aliases.as_ref(), cfg.models.as_ref())
        .expect("карта собирается");
    let want = uwuwu_domain::SpawnModel::parse("p/new-flash#high").unwrap();
    for key in ["default", "developer", "analyst"] {
        assert_eq!(models.get(key), Some(&want), "{key} — новый flash");
    }
    // Назначение tester = flash отлично от канона (common): потеря слоя
    // назначений в клее подменила бы tester каноном и не была бы замечена.
    assert_eq!(
        models.get("tester"),
        Some(&want),
        "неканоническое назначение tester → flash не доехало"
    );
    let common = uwuwu_domain::SpawnModel::parse("p/common#high").unwrap();
    assert_eq!(
        models.model_for_alias(uwuwu_usecase::ModelAlias::Common),
        &common,
        "алиас common не тронут правкой flash"
    );
    let smart = uwuwu_domain::SpawnModel::parse("p/smart#high").unwrap();
    assert_eq!(
        models.get("plan-reviewer"),
        Some(&smart),
        "правка flash не задела smart"
    );
}

/// Связка ExecutorCfg → from_config: неизвестный алиас из TOML — ошибка
/// старта с именем ключа, а не тихий канон.
#[test]
fn unknown_alias_from_toml_is_loud_at_startup() {
    let raw = r#"
        name = "kisa"
        url = "https://example.invalid"

        [model_aliases]
        flash = "opencode-go/hy4-flash-free#high"
        common = "opencode-go/muse-spark-1.3-contributor#high"
        smart = "zai-coding-plan/glm-5.4#high"

        [models]
        default = "flash"
        tester = "turbo"
    "#;
    let cfg: crate::config::ExecutorCfg = toml::from_str(raw).expect("валидный toml");
    let error = SpawnModels::from_config(cfg.model_aliases.as_ref(), cfg.models.as_ref())
        .expect_err("неизвестный алиас отклоняется");
    assert!(error.contains("[executor.models].tester"), "{error}");
    assert!(error.contains("turbo"), "{error}");
}
