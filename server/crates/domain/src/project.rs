//! Проект конвейера: имя + персона + реестр долгосрочных целей (DS:
//! дочерние сущности агрегата). Носитель (доки вики, конфиг деплоя) —
//! вне домена: сегодня вики, завтра БД — агрегат не меняется.

/// Проект конвейера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    name: ProjectName,
    persona: Persona,
    goals: Vec<Goal>,
}

/// Имя проекта (топ-папка вики; непустое).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProjectName(String);

impl ProjectName {
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        (!raw.is_empty() && !raw.contains('/')).then(|| Self(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Персона владельца проекта: компактный профиль, ходит в каждый спавн.
/// Пустая = «работай без персоны» (дока нет) — явное состояние.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Persona(String);

impl Persona {
    pub fn empty() -> Self {
        Self(String::new())
    }

    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    pub fn text(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Долгосрочная цель проекта: заголовок (тело цели ведёт владелец в UI —
/// домен хранит реестр заголовков для сверки направления).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Goal {
    title: String,
}

impl Goal {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into().trim().to_string(),
        }
    }

    pub fn title(&self) -> &str {
        &self.title
    }
}

impl Project {
    pub fn new(name: ProjectName, persona: Persona, goals: Vec<Goal>) -> Self {
        Self {
            name,
            persona,
            goals,
        }
    }

    pub fn name(&self) -> &ProjectName {
        &self.name
    }

    pub fn persona(&self) -> &Persona {
        &self.persona
    }

    pub fn goals(&self) -> &[Goal] {
        &self.goals
    }

    /// Инвариант: цели уникальны по заголовку (дубль — ошибка сборки).
    pub fn validate(&self) -> Result<(), String> {
        let mut seen = std::collections::BTreeSet::new();
        for g in &self.goals {
            if g.title.is_empty() {
                return Err(format!(
                    "проект «{}»: цель с пустым заголовком",
                    self.name.as_str()
                ));
            }
            if !seen.insert(g.title.clone()) {
                return Err(format!(
                    "проект «{}»: цель «{}» задублирована",
                    self.name.as_str(),
                    g.title
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goals_unique() {
        let p = Project::new(
            ProjectName::parse("uwuwu").unwrap(),
            Persona::empty(),
            vec![
                Goal::new("серверная архитектура"),
                Goal::new("серверная архитектура"),
            ],
        );
        assert!(p.validate().is_err());
        let ok = Project::new(
            ProjectName::parse("uwuwu").unwrap(),
            Persona::new("язык: русский"),
            vec![Goal::new("a"), Goal::new("b")],
        );
        assert!(ok.validate().is_ok());
        assert_eq!(ok.persona().text(), "язык: русский");
        assert_eq!(ok.goals().len(), 2);
    }

    #[test]
    fn name_rejects_garbage() {
        assert!(ProjectName::parse("").is_none());
        assert!(ProjectName::parse("a/b").is_none());
        assert!(ProjectName::parse(" uwuwu ").is_some());
    }
}
