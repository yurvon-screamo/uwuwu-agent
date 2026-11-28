//! Уровни моделей (#1684): алиас flash/common/smart указывает на raw-модель
//! один раз, агенты ссылаются на алиас. Порядок вариантов задаёт лестницу,
//! которой воспользуется сдвиг +1/-1 из #1689; здесь сдвига нет.

use std::collections::HashMap;
use uwuwu_domain::SpawnModel;

/// Уровень модели. Порядок объявления — лестница flash < common < smart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelAlias {
    Flash,
    Common,
    Smart,
}

impl ModelAlias {
    pub const ALL: [Self; 3] = [Self::Flash, Self::Common, Self::Smart];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Flash => "flash",
            Self::Common => "common",
            Self::Smart => "smart",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim() {
            "flash" => Ok(Self::Flash),
            "common" => Ok(Self::Common),
            "smart" => Ok(Self::Smart),
            _ => Err(format!(
                "неизвестный алиас модели '{raw}' (flash/common/smart)"
            )),
        }
    }

    /// Сдвиг на `delta` уровней по лестнице ALL с отсечением на границах
    /// (#1689): flash - 1 = flash, smart + 1 = smart. Четвёртого уровня нет.
    pub fn shift(self, delta: i8) -> Self {
        // Исчерпывающий match: новый вариант enum не скомпилируется без
        // явного уровня, паники в пути спавна нет.
        let level = match self {
            Self::Flash => 0i8,
            Self::Common => 1,
            Self::Smart => 2,
        };
        match (level + delta).clamp(0, 2) {
            0 => Self::Flash,
            1 => Self::Common,
            _ => Self::Smart,
        }
    }
}

/// Raw-модели трёх алиасов — единственный носитель raw-модели (#1684).
#[derive(Debug, Clone)]
pub struct ModelAliases {
    flash: SpawnModel,
    common: SpawnModel,
    smart: SpawnModel,
}

impl ModelAliases {
    pub fn new(flash: SpawnModel, common: SpawnModel, smart: SpawnModel) -> Self {
        Self {
            flash,
            common,
            smart,
        }
    }

    /// Канон владельца: smart — как раньше, common с 27.11 — та же raw-модель,
    /// что и flash (решение владельца; совпадение уровней допустимо временно).
    pub fn canonical() -> Self {
        let raw = crate::runtime_cfg::executor::aliases();
        Self::from_map(&raw).expect("config.toml [executor.model_aliases]: валидный канон")
    }

    pub fn get(&self, alias: ModelAlias) -> &SpawnModel {
        match alias {
            ModelAlias::Flash => &self.flash,
            ModelAlias::Common => &self.common,
            ModelAlias::Smart => &self.smart,
        }
    }

    /// `[executor.model_aliases]` → три алиаса. Неполный или лишний раздел
    /// отклоняется: тихой подмены одной из моделей быть не должно.
    pub fn from_map(raw: &HashMap<String, String>) -> Result<Self, String> {
        let mut unknown: Vec<&str> = raw
            .keys()
            .map(String::as_str)
            .filter(|key| ModelAlias::parse(key).is_err())
            .collect();
        unknown.sort_unstable();
        if let Some(key) = unknown.first() {
            return Err(format!(
                "[executor.model_aliases]: неизвестный алиас '{key}' (flash/common/smart)"
            ));
        }
        let missing: Vec<&str> = ModelAlias::ALL
            .iter()
            .map(|alias| alias.as_str())
            .filter(|name| !raw.contains_key(*name))
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "[executor.model_aliases]: нет алиаса '{}'",
                missing.join("', '")
            ));
        }
        let value_of = |alias: ModelAlias| -> Result<SpawnModel, String> {
            let value = raw.get(alias.as_str()).ok_or_else(|| {
                format!("[executor.model_aliases]: нет алиаса {}", alias.as_str())
            })?;
            SpawnModel::parse(value)
                .map_err(|error| format!("[executor.model_aliases].{}: {error}", alias.as_str()))
        };
        Ok(Self::new(
            value_of(ModelAlias::Flash)?,
            value_of(ModelAlias::Common)?,
            value_of(ModelAlias::Smart)?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_shift_clamps_at_flash_and_smart() {
        assert_eq!(ModelAlias::Flash.shift(-1), ModelAlias::Flash);
        assert_eq!(ModelAlias::Flash.shift(1), ModelAlias::Common);
        assert_eq!(ModelAlias::Common.shift(1), ModelAlias::Smart);
        assert_eq!(ModelAlias::Common.shift(-1), ModelAlias::Flash);
        assert_eq!(ModelAlias::Smart.shift(1), ModelAlias::Smart);
        assert_eq!(ModelAlias::Smart.shift(-1), ModelAlias::Common);
        assert_eq!(ModelAlias::Common.shift(0), ModelAlias::Common);
    }
}
