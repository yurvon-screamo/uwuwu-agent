//! Карта моделей спавнов (#1154, решение владельца 17.11): единый
//! механизм конфигурации — модель кладётся сервером в тело спавна
//! (`SessionSpec.model`), деф-файлы агентов моделей не несут. Канон
//! владельца (25.11, #1628): flash-контуры (default/developer/tester/
//! analyst/dreamer) — opencode-go/hy4-flash-free#high, ревьюверы — полный
//! zai-coding-plan/glm-5.4#high (канон 18.11, #417). canonical() следует
//! config.example дословно (кросс-гвард spawn_models_config_sync_test).
//! Ключи карты — имена агентов (naming::agent_name) + "default"
//! (bootstrap-сессии подготовки env; до #1300 — также дев-семья, после
//! пина агента Design/Dev берут ключ "developer").

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
    /// Синхрон с config.example (кросс-гвард): flash-контуры на
    /// opencode-go/hy4-flash-free#high (25.11, #1628 — supersedes санкцию
    /// 24.11 в #1607 по ключу analyst), ревьюверы стадий + ревью
    /// аналитики — zai-coding-plan/glm-5.4#high (поручение 18.11, f1b9091c).
    pub fn canonical() -> Self {
        let mut map = HashMap::new();
        let mut put = |key: &str, raw: &str| {
            map.insert(
                key.to_string(),
                SpawnModel::parse(raw).expect("канон-карта"),
            );
        };
        put("default", "opencode-go/hy4-flash-free#high");
        put("developer", "opencode-go/hy4-flash-free#high");
        put("plan-reviewer", "zai-coding-plan/glm-5.4#high");
        put("code-quality-reviewer", "zai-coding-plan/glm-5.4#high");
        put("tester", "opencode-go/hy4-flash-free#high");
        put("test-reviewer", "zai-coding-plan/glm-5.4#high");
        put("analyst", "opencode-go/hy4-flash-free#high");
        put("analyst-reviewer", "zai-coding-plan/glm-5.4#high");
        put("dreamer", "opencode-go/hy4-flash-free#high");
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

    /// Модель bootstrap-сессий подготовки env (ключ "default"). Рабочие
    /// дев-сессии с #1300 берут ключ "developer" (for_agent) — переход
    /// осознанный, ключи карты канонически одинаковы.
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
        // канон владельца 25.11 (#1628): flash-контуры на
        // opencode-go/hy4-flash-free#high (supersedes санкцию 24.11 в #1607
        // по ключу analyst), ревьюверы — glm-5.4#high;
        // карта = config.example verbatim (кросс-гвард)
        let m = SpawnModels::canonical();
        assert_eq!(
            m.for_agent(Agent::PlanReviewer),
            SpawnModel::parse("zai-coding-plan/glm-5.4#high").unwrap(),
            "ревьюверы — полный glm-5.4#high (канон #417)"
        );
        assert_eq!(
            m.for_agent(Agent::Analyst),
            SpawnModel::parse("opencode-go/hy4-flash-free#high").unwrap(),
            "аналитик — flash-free (санкция 25.11 в #1628, без исключений)"
        );
        assert_eq!(
            m.for_agent(Agent::AnalystReviewer),
            SpawnModel::parse("zai-coding-plan/glm-5.4#high").unwrap(),
            "ревью аналитики — glm-5.4#high (санкция 24.11, #1607)"
        );
        assert_eq!(
            m.for_agent(Agent::Dreamer),
            SpawnModel::parse("opencode-go/hy4-flash-free#high").unwrap()
        );
        assert_eq!(
            m.for_agent(Agent::Tester),
            SpawnModel::parse("opencode-go/hy4-flash-free#high").unwrap()
        );
        assert_eq!(m.for_default(), m.for_agent(Agent::Developer));
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
