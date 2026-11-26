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
