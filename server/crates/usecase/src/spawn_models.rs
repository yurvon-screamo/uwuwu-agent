//! Карта моделей спавнов (#1154, решение владельца 17.11): единый
//! механизм конфигурации — модель кладётся сервером в тело спавна
//! (`SessionSpec.model`), деф-файлы агентов моделей не несут. Канон
//! владельца (11.11 «качественные контуры не режутся» + 1135 «Ревьювер —
//! полный 5.3»): качественные — полный glm-5.3, рутинные — flash.
//! Ключи карты — имена агентов (naming::agent_name) + "default"
//! (Dev/Design/bootstrap — default_agent опенкода — developer).

use crate::naming;
use std::collections::HashMap;
use uwuwu_domain::{Agent, SpawnModel};

#[derive(Debug, Clone)]
pub struct SpawnModels {
    map: HashMap<String, SpawnModel>,
}

impl SpawnModels {
    pub fn new(map: HashMap<String, SpawnModel>) -> Self {
        Self { map }
    }

    /// Канон владельца — дефолт карты при отсутствии секции в конфиге.
    /// Синхрон с config.example (кросс-гвард): везде flash#high,
    /// ревьюверы полный 5.3#high (поручение владельца 18.11, f1b9091c).
    pub fn canonical() -> Self {
        let mut map = HashMap::new();
        let mut put = |key: &str, raw: &str| {
            map.insert(
                key.to_string(),
                SpawnModel::parse(raw).expect("канон-карта"),
            );
        };
        put("default", "zai-coding-plan/glm-5.3-flash#high");
        put("developer", "zai-coding-plan/glm-5.3-flash#high");
        put("plan-reviewer", "zai-coding-plan/glm-5.3#high");
        put("code-quality-reviewer", "zai-coding-plan/glm-5.3#high");
        put("tester", "zai-coding-plan/glm-5.3-flash#high");
        put("analyst", "zai-coding-plan/glm-5.3-flash#high");
        put("analyst-reviewer", "zai-coding-plan/glm-5.3#high");
        put("dreamer", "zai-coding-plan/glm-5.3-flash#high");
        Self { map }
    }

    /// Множество ключей карты (агенты + default) — кросс-гвард с конфигом.
    pub fn keys(&self) -> Vec<&str> {
        let mut keys: Vec<&str> = self.map.keys().map(String::as_str).collect();
        keys.sort_unstable();
        keys
    }

    /// Модель агента конвейера (по имени из naming::agent_name).
    pub fn for_agent(&self, agent: Agent) -> SpawnModel {
        self.by_key(naming::agent_name(agent))
            .cloned()
            .unwrap_or_else(|| self.fallback(agent))
    }

    /// Значение карты по ключу (кросс-гварды конфига и тесты).
    pub fn get(&self, key: &str) -> Option<&SpawnModel> {
        self.map.get(key)
    }

    /// Модель default-слота: Dev/Design (default_agent опенкода —
    /// developer) и bootstrap-сессии подготовки env.
    pub fn for_default(&self) -> SpawnModel {
        self.by_key("default")
            .cloned()
            .unwrap_or_else(|| self.fallback(Agent::Developer))
    }

    fn by_key(&self, key: &str) -> Option<&SpawnModel> {
        self.map.get(key)
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
        // канон несёт все ключи агентов — expect достижим только при
        // недействительном agent_name (невозможно: ключи из naming)
        SpawnModels::canonical()
            .map
            .get(key)
            .cloned()
            .expect("канон несёт ключ агента")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_carries_owner_model_map() {
        // канон владельца 18.11 (#417): везде flash#high, ревьюверы —
        // полный 5.3#high; карта = config.example verbatim (кросс-гвард)
        let m = SpawnModels::canonical();
        assert_eq!(
            m.for_agent(Agent::PlanReviewer),
            SpawnModel::parse("zai-coding-plan/glm-5.3#high").unwrap(),
            "ревьюверы — полный 5.3#high (канон #417)
        );
        assert_eq!(
            m.for_agent(Agent::Dreamer),
            SpawnModel::parse("zai-coding-plan/glm-5.3-flash#high").unwrap()
        );
        assert_eq!(
            m.for_agent(Agent::Tester),
            SpawnModel::parse("zai-coding-plan/glm-5.3-flash#high").unwrap()
        );
        assert_eq!(m.for_default(), m.for_agent(Agent::Developer));
    }

    #[test]
    fn model_string_parses_with_variant() {
        let sm = SpawnModel::parse("zai-coding-plan/glm-5.3-flash#max").unwrap();
        assert_eq!(sm.model.provider(), "zai-coding-plan");
        assert_eq!(sm.model.id(), "glm-5.3-flash");
        assert_eq!(sm.variant.as_deref(), Some("max"));

        let plain = SpawnModel::parse("p/id").unwrap();
        assert_eq!(plain.variant, None);
        assert!(SpawnModel::parse("без-слэша").is_err());
        assert!(SpawnModel::parse("/id").is_err());
    }

    #[test]
    fn missing_key_falls_back_to_canon_with_warn() {
        let mut map = HashMap::new();
        map.insert("default".to_string(), SpawnModel::parse("p/id").unwrap());
        let m = SpawnModels::new(map);
        // громкий фоллбэк (warn в лог), не тихий: значение = канон
        assert_eq!(
            m.for_agent(Agent::Tester),
            SpawnModels::canonical().for_agent(Agent::Tester)
        );
    }
}
