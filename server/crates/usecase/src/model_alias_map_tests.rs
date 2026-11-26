//! Как agents из карты моделей (#1154) читают алиасы (#1684). Тесты
//! контракта `SpawnModels`: раскладка, матрица секций, громкие отказы.

use crate::model_aliases::{ModelAlias, ModelAliases};
use crate::spawn_models::{canonical_assignments, SpawnModels};
use std::collections::HashMap;
use uwuwu_domain::{Agent, SpawnModel};

fn strings(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

fn canonical_raws() -> HashMap<String, String> {
    strings(&[
        ("flash", "opencode-go/hy4-flash-free#high"),
        ("common", "opencode-go/muse-spark-1.3-contributor#high"),
        ("smart", "zai-coding-plan/glm-5.4#high"),
    ])
}

fn canonical_assignment_raws() -> HashMap<String, String> {
    strings(&[
        ("default", "flash"),
        ("developer", "flash"),
        ("plan-reviewer", "smart"),
        ("code-quality-reviewer", "smart"),
        ("tester", "common"),
        ("test-reviewer", "smart"),
        ("analyst", "flash"),
        ("analyst-reviewer", "smart"),
        ("dreamer", "common"),
    ])
}

#[test]
fn canonical_carries_owner_model_map() {
    let models = SpawnModels::canonical();
    let flash = SpawnModel::parse("opencode-go/hy4-flash-free#high").unwrap();
    let common = SpawnModel::parse("opencode-go/muse-spark-1.3-contributor#high").unwrap();
    let smart = SpawnModel::parse("zai-coding-plan/glm-5.4#high").unwrap();

    for agent in [Agent::Developer, Agent::Analyst] {
        assert_eq!(models.for_agent(agent), flash, "{agent:?} — flash");
    }
    for agent in [Agent::Tester, Agent::Dreamer] {
        assert_eq!(models.for_agent(agent), common, "{agent:?} — common");
    }
    for agent in [
        Agent::PlanReviewer,
        Agent::CodeQualityReviewer,
        Agent::TestReviewer,
        Agent::AnalystReviewer,
    ] {
        assert_eq!(models.for_agent(agent), smart, "{agent:?} — smart");
    }
    assert_eq!(models.for_default(), flash, "bootstrap — flash");
}

#[test]
fn alias_level_stays_available_at_runtime() {
    let models = SpawnModels::canonical();
    assert_eq!(models.alias_for_agent(Agent::Tester), ModelAlias::Common);
    assert_eq!(
        models.alias_for_agent(Agent::PlanReviewer),
        ModelAlias::Smart
    );
    assert_eq!(models.alias_for_agent(Agent::Analyst), ModelAlias::Flash);
    assert_eq!(models.alias_for_key("default"), Some(ModelAlias::Flash));
    assert_eq!(
        models.model_for_alias(ModelAlias::Common),
        &SpawnModel::parse("opencode-go/muse-spark-1.3-contributor#high").unwrap()
    );
}

#[test]
fn absent_sections_fall_back_to_full_canon() {
    let models = SpawnModels::from_config(None, None).unwrap();
    assert_eq!(
        models.for_agent(Agent::Tester),
        SpawnModels::canonical().for_agent(Agent::Tester)
    );
    assert_eq!(
        models.for_agent(Agent::Developer),
        SpawnModels::canonical().for_agent(Agent::Developer)
    );
}

#[test]
fn aliases_only_keep_canonical_assignments() {
    let aliases = strings(&[
        ("flash", "p/new-flash#high"),
        ("common", "p/common#high"),
        ("smart", "p/smart#high"),
    ]);
    let models = SpawnModels::from_config(Some(&aliases), None).unwrap();
    let want = SpawnModel::parse("p/new-flash#high").unwrap();
    assert_eq!(
        models.for_agent(Agent::Developer),
        want,
        "Разработчик — новый flash"
    );
    assert_eq!(models.for_default(), want, "bootstrap — новый flash");
    assert_eq!(
        models.for_agent(Agent::Tester),
        SpawnModel::parse("p/common#high").unwrap()
    );
}

#[test]
fn assignments_only_keep_canonical_aliases() {
    let assignments = strings(&[
        ("default", "flash"),
        ("developer", "smart"),
        ("tester", "flash"),
    ]);
    let models = SpawnModels::from_config(None, Some(&assignments)).unwrap();
    assert_eq!(
        models.for_agent(Agent::Developer),
        SpawnModel::parse("zai-coding-plan/glm-5.4#high").unwrap()
    );
    assert_eq!(
        models.for_agent(Agent::Tester),
        SpawnModel::parse("opencode-go/hy4-flash-free#high").unwrap()
    );
}

#[test]
fn both_sections_resolve_to_flat_models() {
    let models =
        SpawnModels::from_config(Some(&canonical_raws()), Some(&canonical_assignment_raws()))
            .unwrap();
    let canonical = SpawnModels::canonical();
    assert_eq!(models.keys(), canonical.keys());
    for key in canonical.keys() {
        assert_eq!(models.alias_for_key(key), canonical.alias_for_key(key));
        assert_eq!(models.get(key), canonical.get(key));
    }
}

#[test]
fn unknown_alias_in_assignment_is_loud() {
    let assignments = strings(&[("tester", "turbo")]);
    let error = SpawnModels::from_config(None, Some(&assignments)).unwrap_err();
    assert!(error.contains("[executor.models].tester"), "{error}");
    assert!(error.contains("turbo"), "{error}");
}

#[test]
fn incomplete_alias_section_names_missing_key() {
    let aliases = strings(&[("flash", "p/flash#high"), ("common", "p/common#high")]);
    let error = SpawnModels::from_config(Some(&aliases), None).unwrap_err();
    assert!(error.contains("executor.model_aliases"), "{error}");
    assert!(error.contains("smart"), "{error}");
}

#[test]
fn extra_alias_key_is_loud() {
    let mut aliases = canonical_raws();
    aliases.insert("turbo".into(), "p/turbo#high".into());
    let error = SpawnModels::from_config(Some(&aliases), None).unwrap_err();
    assert!(error.contains("turbo"), "{error}");
}

#[test]
fn bad_raw_in_alias_section_names_alias() {
    let mut aliases = canonical_raws();
    aliases.insert("smart".into(), "без-слэша".into());
    let error = SpawnModels::from_config(Some(&aliases), None).unwrap_err();
    assert!(error.contains("[executor.model_aliases].smart"), "{error}");
}

#[test]
fn missing_agent_key_falls_back_to_canon() {
    let aliases = ModelAliases::canonical();
    let assignments = HashMap::from([("default".to_string(), ModelAlias::Flash)]);
    let models = SpawnModels::new(aliases, assignments);
    assert_eq!(
        models.for_agent(Agent::Tester),
        SpawnModels::canonical().for_agent(Agent::Tester)
    );
    assert_eq!(models.for_default(), SpawnModels::canonical().for_default());
}

#[test]
fn missing_default_key_logs_key_missing_and_uses_canon() {
    use crate::pipeline::degraded_list_support::{has_exact_field, trap_events};
    let aliases = ModelAliases::canonical();
    let assignments = HashMap::from([("developer".to_string(), ModelAlias::Flash)]);
    let models = SpawnModels::new(aliases, assignments);

    let (guard, records) = trap_events();
    let model = models.for_default();
    drop(guard);

    assert_eq!(model, SpawnModels::canonical().for_default());
    assert!(
        has_exact_field(
            &records.lock().unwrap(),
            "event",
            "spawn_models.key_missing"
        ),
        "пропуск default — громкий warn, не тихая подмена"
    );
}

#[test]
fn model_string_parses_with_variant() {
    let sm = SpawnModel::parse("opencode-go/hy4-flash-free#max").unwrap();
    assert_eq!(sm.model.provider(), "opencode-go");
    assert_eq!(sm.model.id(), "hy4-flash-free");
    assert_eq!(sm.variant.as_deref(), Some("max"));
    let plain = SpawnModel::parse("p/id").unwrap();
    assert_eq!(plain.variant, None);
    assert!(SpawnModel::parse("без-слэша").is_err());
    assert!(SpawnModel::parse("/id").is_err());
}

#[test]
fn one_alias_edit_changes_whole_group() {
    let mut aliases = canonical_raws();
    aliases.insert("flash".into(), "p/new-flash#high".into());
    let models = SpawnModels::from_config(Some(&aliases), None).unwrap();
    let want = SpawnModel::parse("p/new-flash#high").unwrap();
    for key in ["default", "developer", "analyst"] {
        assert_eq!(models.get(key), Some(&want), "{key} — новый flash");
    }
    assert_eq!(
        models.get("tester"),
        SpawnModels::canonical().get("tester"),
        "tester не задет правкой flash"
    );
}

#[test]
fn canonical_assignments_cover_every_agent() {
    let assignments = canonical_assignments();
    for agent in [
        Agent::Developer,
        Agent::PlanReviewer,
        Agent::CodeQualityReviewer,
        Agent::Tester,
        Agent::TestReviewer,
        Agent::Analyst,
        Agent::AnalystReviewer,
        Agent::Dreamer,
    ] {
        assert!(
            assignments.contains_key(crate::naming::agent_name(agent)),
            "нет назначения для {agent:?}"
        );
    }
}
