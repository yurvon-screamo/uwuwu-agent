//! Кросс-гвард канона моделей (#1154): canonical() ↔ [executor.models]
//! config.example.toml совпадают verbatim — «канон владельца» не
//! разъезжается между кодом и примером конфига (rev. ревью R2: заявка
//! синхрона без теста была несуществующей гарантией).

use std::collections::HashMap;
use uwuwu_domain::SpawnModel;
use uwuwu_usecase::SpawnModels;

#[test]
fn canonical_matches_config_example_verbatim() {
    // Arrange: живой config.example.toml из корня server/
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config.example.toml");
    let raw = std::fs::read_to_string(&path).expect("config.example.toml читается");
    let value: toml::Value = raw.parse().expect("config.example.toml валидный toml");
    let section = value["executor"]["models"]
        .as_table()
        .expect("секция [executor.models] присутствует");

    // Act
    let canonical = SpawnModels::canonical();
    let mut map: HashMap<String, SpawnModel> = HashMap::new();
    for (key, model) in section {
        let raw = model.as_str().expect("значение карты — строка");
        map.insert(key.clone(), SpawnModel::parse(raw).expect("парсится"));
    }

    // Assert: множества и значения совпадают в обе стороны
    let example = SpawnModels::new(map);
    for agent in [
        uwuwu_domain::Agent::Developer,
        uwuwu_domain::Agent::PlanReviewer,
        uwuwu_domain::Agent::CodeQualityReviewer,
        uwuwu_domain::Agent::Tester,
        uwuwu_domain::Agent::Analyst,
        uwuwu_domain::Agent::AnalystReviewer,
        uwuwu_domain::Agent::Dreamer,
    ] {
        assert_eq!(
            example.for_agent(agent),
            canonical.for_agent(agent),
            "config.example ↔ canonical: {agent:?} разошлись"
        );
    }
    assert_eq!(
        example.for_default(),
        canonical.for_default(),
        "config.example ↔ canonical: default разошлись"
    );
}
