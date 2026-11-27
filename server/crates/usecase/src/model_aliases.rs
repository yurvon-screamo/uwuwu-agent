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
        let index = Self::ALL
            .iter()
            .position(|alias| *alias == self)
            .expect("каждый алиас есть в ALL") as i8;
        let shifted = (index + delta).clamp(0, Self::ALL.len() as i8 - 1) as usize;
        Self::ALL[shifted]
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

    /// Канон владельца 26.11 (#1684): flash и smart — как раньше,
    /// common — muse-spark-1.3-contributor (сверен живым каталогом).
    pub fn canonical() -> Self {
        Self::new(
            SpawnModel::parse("opencode-go/hy4-flash-free#high").expect("канон-алиас flash"),
            SpawnModel::parse("opencode-go/muse-spark-1.3-contributor#high")
                .expect("канон-алиас common"),
            SpawnModel::parse("zai-coding-plan/glm-5.4#high").expect("канон-алиас smart"),
        )
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
