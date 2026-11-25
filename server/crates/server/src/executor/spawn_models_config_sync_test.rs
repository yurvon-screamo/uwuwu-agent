//! Кросс-гвард канона моделей (#1154): canonical() ↔ [executor.models]
//! config.example.toml И живого config.toml совпадают verbatim — «канон
//! владельца» не разъезжается между кодом и конфигами (Low-1 #1607:
//! Dockerfile копирует именно живой config.toml, гардом он не покрыт —
//! забытый живой файл = зелёные тесты и flash в проде).

use std::collections::HashMap;
use uwuwu_domain::SpawnModel;
use uwuwu_usecase::SpawnModels;

/// Карта [executor.models] из TOML-файла (пример или живой конфиг).
fn models_of(path: &std::path::Path) -> HashMap<String, SpawnModel> {
    let raw =
        std::fs::read_to_string(path).unwrap_or_else(|_| panic!("{} читается", path.display()));
    let value: toml::Value = raw.parse().expect("валидный toml");
    let section = value["executor"]["models"]
        .as_table()
        .expect("секция [executor.models] присутствует");
    let mut map: HashMap<String, SpawnModel> = HashMap::new();
    for (key, model) in section {
        let raw = model.as_str().expect("значение карты — строка");
        map.insert(key.clone(), SpawnModel::parse(raw).expect("парсится"));
    }
    map
}

/// Сверка одной карты с каноном: ключи + значения + for_agent-проход.
fn assert_matches_canonical(map: HashMap<String, SpawnModel>, origin: &str) {
    let canonical = SpawnModels::canonical();
    let mut example_keys_raw: Vec<&String> = map.keys().collect();
    example_keys_raw.sort();
    let example_keys: Vec<&str> = example_keys_raw.iter().map(|k| k.as_str()).collect();
    // множества ключей совпадают — без фоллбэка for_agent: rogue-ключ
    // и отсутствующий ключ ловятся здесь, а не маскируются канон-дефолтом
    assert_eq!(
        example_keys,
        canonical.keys(),
        "{origin} [executor.models] ≠ canonical: ключи разошлись"
    );
    // значения — напрямую, не через for_agent (фоллбэк for_agent
    // маскировал бы рассинхрон значения)
    let example = SpawnModels::new(map.clone());
    for key in &canonical.keys() {
        assert_eq!(
            example.get(key),
            canonical.get(key),
            "{origin} ↔ canonical: значение '{key}' разошлось"
        );
    }
    // для_agent-проход (суммарный контракт конвейера)
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
            example.for_agent(agent),
            canonical.for_agent(agent),
            "{origin} ↔ canonical: {agent:?} разошлись"
        );
    }
    assert_eq!(
        example.for_default(),
        canonical.for_default(),
        "{origin} ↔ canonical: default разошлись"
    );
}

#[test]
fn canonical_matches_config_example_verbatim() {
    // Arrange: живой config.example.toml из корня server/
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config.example.toml");

    // Act + Assert
    assert_matches_canonical(models_of(&path), "config.example");
}

#[test]
fn canonical_matches_live_config_verbatim() {
    // Arrange: живой config.toml из корня server/ — именно его копирует
    // Dockerfile в прод (Low-1 #1607)
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config.toml");

    // Act + Assert
    assert_matches_canonical(models_of(&path), "config.toml");
}
