//! Поведенческие проверки карты моделей (uwuwu#1726): тесты-пины
//! дословного совпадения config.toml/config.example.toml с каноном кода
//! сняты — значения живут только в конфиге. Остались проверки поведения:
//! inline TOML → ExecutorCfg → from_config → плоская карта.

use uwuwu_usecase::SpawnModels;

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
        tester = "common"
        plan-reviewer = "smart"
    "#;
    let cfg: crate::config::ExecutorCfg = toml::from_str(raw).expect("валидный toml");
    let models = SpawnModels::from_config(cfg.model_aliases.as_ref(), cfg.models.as_ref())
        .expect("карта собирается");
    let want = uwuwu_domain::SpawnModel::parse("p/new-flash#high").unwrap();
    let common = uwuwu_domain::SpawnModel::parse("p/common#high").unwrap();
    for key in ["default", "developer", "analyst"] {
        assert_eq!(models.get(key), Some(&want), "{key} — новый flash");
    }
    // Назначение tester = common отлично от канона (flash): потеря слоя
    // назначений в клее подменила бы tester каноном и не была бы замечена.
    assert_eq!(
        models.get("tester"),
        Some(&common),
        "неканоническое назначение tester → common не доехало"
    );
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
        common = "opencode-go/hy4-flash-free#high"
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
