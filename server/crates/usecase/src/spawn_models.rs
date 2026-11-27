//! Карта моделей спавнов (#1154, #1684): raw-модель живёт на алиасе
//! flash/common/smart, агенты и `default` ссылаются на алиас. Сервер кладёт
//! модель в тело спавна (`SessionSpec.model`), деф-файлы агентов моделей
//! не несут. Уровень алиаса доступен в рантайме для сдвига из #1689.

use crate::model_aliases::{ModelAlias, ModelAliases};
use crate::naming;
use std::collections::HashMap;
use uwuwu_domain::{Agent, Complexity, SpawnModel, SpawnPolicy};

/// Провайдер peak-окна живёт в `[pipeline] peak_provider` config.toml (uwuwu#1726).
pub fn peak_provider() -> String {
    crate::runtime_cfg::pipeline::peak_provider()
}

/// Peak-гейт применяется только к этапам целевого провайдера.
pub fn peak_applies(model: &SpawnModel) -> bool {
    model.model.provider() == peak_provider()
}

/// Один механизм фильтра пика для всех четырёх точек гейта (#1680/#1689):
/// нецелевой провайдер работает в окне. Решение — через `peak_applies`,
/// без собственного литерала провайдера; `SpawnPolicy` копируется.
pub fn peak_policy_for_model(policy: SpawnPolicy, model: &SpawnModel) -> SpawnPolicy {
    if peak_applies(model) {
        policy
    } else {
        let mut filtered = policy;
        filtered.peak.enabled = false;
        filtered
    }
}

/// Раскладка владельца: 26.11 (#1684) flash — default/developer/analyst,
/// common — tester/dreamer, smart — ревьюверы; 27.11 (#1693) tester → flash,
/// живой конфиг не менялся, канон и `config.example.toml` следуют за ним.
pub fn canonical_assignments() -> HashMap<String, ModelAlias> {
    crate::runtime_cfg::executor::models()
        .into_iter()
        .map(|(key, value)| {
            (
                key.clone(),
                ModelAlias::parse(&value)
                    .unwrap_or_else(|error| panic!("config.toml [executor.models].{key}: {error}")),
            )
        })
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

    /// Сдвиг сложности (#1689): алиас ключа сдвигается на уровень до
    /// выбора raw-модели. Приоритет сюда не передаётся — только Complexity.
    pub fn for_key_shifted(&self, key: &str, complexity: Complexity) -> SpawnModel {
        let alias = self.alias_or_canon(key);
        self.model_for_alias(alias.shift(complexity.tier_delta()))
            .clone()
    }

    /// Уровень, на котором реально уходит сессия — для поля `alias` в логах.
    /// Одна семантика на всех пяти событиях спавна/гейта (#1689 ревью).
    pub fn alias_for_key_shifted(&self, key: &str, complexity: Complexity) -> ModelAlias {
        self.alias_or_canon(key).shift(complexity.tier_delta())
    }

    /// То же для агента конвейера: один вызов для всех стадийных спавнов.
    pub fn for_agent_shifted(&self, agent: Agent, complexity: Complexity) -> SpawnModel {
        self.for_key_shifted(naming::agent_name(agent), complexity)
    }

    /// Модель агента конвейера (по имени из naming::agent_name).
    pub fn for_agent(&self, agent: Agent) -> SpawnModel {
        self.get(naming::agent_name(agent))
            .cloned()
            .unwrap_or_else(|| self.fallback(naming::agent_name(agent)))
    }

    /// Модель bootstrap-сессий подготовки env (ключ "default"). Рабочие
    /// дев-сессии берут ключ "developer" (for_agent).
    pub fn for_default(&self) -> SpawnModel {
        self.get("default")
            .cloned()
            .unwrap_or_else(|| self.fallback("default"))
    }

    /// Единственный фолбэк «ключа нет в карте» для всех резолвов: один warn,
    /// канон-алиас, а для ключа вне канона — flash без паники (#1689 ревью).
    fn alias_or_canon(&self, key: &str) -> ModelAlias {
        if let Some(alias) = self.alias_for_key(key) {
            return alias;
        }
        tracing::warn!(
            event = "spawn_models.key_missing",
            key = %key,
            "Карта моделей: нет ключа — секция [executor.models] неполна, \
             применён канон-алиас"
        );
        canonical_assignments()
            .get(key)
            .copied()
            .unwrap_or_else(|| {
                tracing::warn!(
                    event = "spawn_models.key_unknown",
                    key = %key,
                    "Ключ вне канона — применён базовый flash"
                );
                ModelAlias::Flash
            })
    }

    /// Отсутствие ключа — не тихий дефолт: громкий warn + канон. Падать
    /// нельзя — один неполный ключ не должен ронять весь конвейер.
    fn fallback(&self, key: &str) -> SpawnModel {
        let alias = self.alias_or_canon(key);
        Self::canonical().model_for_alias(alias).clone()
    }
}
