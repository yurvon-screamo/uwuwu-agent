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
        if raw.len() != ModelAlias::ALL.len() {
            return Err(format!(
                "[executor.model_aliases]: ожидаются flash/common/smart, получено ключей {}",
                raw.len()
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
