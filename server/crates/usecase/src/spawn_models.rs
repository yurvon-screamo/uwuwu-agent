//! Карта моделей спавнов (#1154, #1684): raw-модель живёт на алиасе
//! flash/common/smart, агенты и `default` ссылаются на алиас. Сервер кладёт
//! модель в тело спавна (`SessionSpec.model`), деф-файлы агентов моделей
//! не несут. Уровень алиаса доступен в рантайме для сдвига из #1689.

use crate::model_aliases::{ModelAlias, ModelAliases};
use crate::naming;
use std::collections::HashMap;
use uwuwu_domain::{Agent, SpawnModel};

/// Провайдер, для которого действует peak-окно (#1679/#1680).
pub const PEAK_PROVIDER: &str = "zai-coding-plan";

/// Peak-гейт применяется только к этапам целевого провайдера.
pub fn peak_applies(model: &SpawnModel) -> bool {
    model.model.provider() == PEAK_PROVIDER
}

/// Раскладка опроса владельца 26.11 (#1684): flash — default/developer/
/// analyst, common — tester/dreamer, smart — все ревьюверы.
pub fn canonical_assignments() -> HashMap<String, ModelAlias> {
    HashMap::from([
        ("default", ModelAlias::Flash),
        ("developer", ModelAlias::Flash),
        ("plan-reviewer", ModelAlias::Smart),
        ("code-quality-reviewer", ModelAlias::Smart),
        ("tester", ModelAlias::Common),
        ("test-reviewer", ModelAlias::Smart),
        ("analyst", ModelAlias::Flash),
        ("analyst-reviewer", ModelAlias::Smart),
        ("dreamer", ModelAlias::Common),
    ])
    .into_iter()
    .map(|(key, alias)| (key.to_string(), alias))
    .collect()
}

#[derive(Debug, Clone)]
pub struct SpawnModels {
    aliases: ModelAliases,
    assignments: HashMap<String, ModelAlias>,
}

impl SpawnModels {
    pub fn new(aliases: ModelAliases, assignments: HashMap<String, ModelAlias>) -> Self {
        Self {
            aliases,
            assignments,
        }
    }

    /// Два необязательных раздела конфига. Каждый отсутствующий слой берётся
    /// из канона; неизвестный алиас в назначении — громкий отказ старта.
    pub fn from_config(
        alias_raw: Option<&HashMap<String, String>>,
        assignment_raw: Option<&HashMap<String, String>>,
    ) -> Result<Self, String> {
        let aliases = match alias_raw {
            Some(raw) => ModelAliases::from_map(raw)?,
            None => ModelAliases::canonical(),
        };
        let assignments = match assignment_raw {
            Some(raw) => raw
                .iter()
                .map(|(key, value)| {
                    ModelAlias::parse(value)
                        .map(|alias| (key.clone(), alias))
                        .map_err(|error| format!("[executor.models].{key}: {error}"))
                })
                .collect::<Result<HashMap<String, ModelAlias>, String>>()?,
            None => canonical_assignments(),
        };
        Ok(Self::new(aliases, assignments))
    }

    pub fn canonical() -> Self {
        Self::new(ModelAliases::canonical(), canonical_assignments())
    }

    /// Ключи карты (агенты + default) — кросс-гвард с конфигом.
    pub fn keys(&self) -> Vec<&str> {
        let mut keys: Vec<&str> = self.assignments.keys().map(String::as_str).collect();
        keys.sort_unstable();
        keys
    }

    /// Модель по ключу: алиас назначения → raw-модель алиаса.
    pub fn get(&self, key: &str) -> Option<&SpawnModel> {
        self.assignments
            .get(key)
            .map(|alias| self.aliases.get(*alias))
    }

    /// Алиас ключа — доступен в рантайме для сдвига уровня (#1689).
    pub fn alias_for_key(&self, key: &str) -> Option<ModelAlias> {
        self.assignments.get(key).copied()
    }

    pub fn alias_for_agent(&self, agent: Agent) -> ModelAlias {
        self.alias_for_key(naming::agent_name(agent))
            .unwrap_or_else(|| canonical_assignments()[naming::agent_name(agent)])
    }

    pub fn model_for_alias(&self, alias: ModelAlias) -> &SpawnModel {
        self.aliases.get(alias)
    }

    /// Модель агента конвейера (по имени из naming::agent_name).
    pub fn for_agent(&self, agent: Agent) -> SpawnModel {
        self.get(naming::agent_name(agent))
            .cloned()
            .unwrap_or_else(|| self.fallback(agent))
    }

    /// Модель bootstrap-сессий подготовки env (ключ "default"). Рабочие
    /// дев-сессии берут ключ "developer" (for_agent).
    pub fn for_default(&self) -> SpawnModel {
        self.get("default")
            .cloned()
            .unwrap_or_else(|| self.fallback(Agent::Developer))
    }

    /// Отсутствие ключа — не тихий дефолт: громкий warn + канон. Падать
    /// нельзя — один неполный ключ не должен ронять весь конвейер.
    fn fallback(&self, agent: Agent) -> SpawnModel {
        let key = naming::agent_name(agent);
        tracing::warn!(
            event = "spawn_models.key_missing",
            key = %key,
            "Карта моделей: нет ключа — секция [executor.models] неполна, \
             применён канон-дефолт"
        );
        Self::canonical()
            .get(key)
            .cloned()
            .expect("канон несёт ключ агента")
    }
}
