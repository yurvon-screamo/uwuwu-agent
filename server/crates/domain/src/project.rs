//! Проект конвейера: имя + доска + репо + чекаут + env-корень.
//! Инварианты проверяются при сборке реестра — битый конфиг валит старт
//! сервера громко, а не молчит пустыми строками в рантайме.

/// Проект конвейера (строка [[project]] конфига).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    name: String,
    board: String,
    repo: String,
    checkout: String,
    env_root: String,
}

impl Project {
    pub fn new(
        name: impl Into<String>,
        board: impl Into<String>,
        repo: impl Into<String>,
        checkout: impl Into<String>,
        env_root: impl Into<String>,
    ) -> Result<Self, String> {
        let p = Self {
            name: name.into().trim().to_string(),
            board: board.into().trim().to_string(),
            repo: repo.into().trim().to_string(),
            checkout: checkout.into().trim().to_string(),
            env_root: env_root.into().trim().to_string(),
        };
        if p.name.is_empty() {
            return Err("проект без имени".into());
        }
        if p.board.is_empty() {
            return Err(format!("проект «{}»: пустая доска", p.name));
        }
        if p.repo.split('/').count() != 2 {
            return Err(format!(
                "проект «{}»: repo «{}» не в форме owner/name",
                p.name, p.repo
            ));
        }
        if p.checkout.is_empty() || p.env_root.is_empty() {
            return Err(format!(
                "проект «{}»: пустой checkout/env_root (спавн невозможен)",
                p.name
            ));
        }
        Ok(p)
    }

    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn board(&self) -> &str {
        &self.board
    }
    pub fn repo(&self) -> &str {
        &self.repo
    }
    pub fn checkout(&self) -> &str {
        &self.checkout
    }
    pub fn env_root(&self) -> &str {
        &self.env_root
    }
}

/// Реестр проектов: имена уникальны (доски может делить — имя нет).
#[derive(Debug, Clone, Default)]
pub struct ProjectRegistry {
    projects: Vec<Project>,
}

impl ProjectRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Добавить проект; дубль имени — ошибка.
    pub fn add(&mut self, project: Project) -> Result<(), String> {
        if self.projects.iter().any(|p| p.name == project.name) {
            return Err(format!("проект «{}» уже в реестре", project.name()));
        }
        self.projects.push(project);
        Ok(())
    }

    pub fn iter(&self) -> impl Iterator<Item = &Project> {
        self.projects.iter()
    }

    pub fn len(&self) -> usize {
        self.projects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.projects.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_project(name: &str) -> Project {
        Project::new(name, "Задачи x", "owner/repo", "/co", "/env").expect("валидный проект")
    }

    #[test]
    fn rejects_broken_config() {
        assert!(Project::new("", "B", "o/r", "/c", "/e").is_err());
        assert!(Project::new("p", "", "o/r", "/c", "/e").is_err());
        assert!(
            Project::new("p", "B", "repo", "/c", "/e").is_err(),
            "repo без owner/"
        );
        assert!(Project::new("p", "B", "o/r", "", "/e").is_err());
    }

    #[test]
    fn registry_rejects_dup_names() {
        let mut r = ProjectRegistry::new();
        r.add(ok_project("uwuwu")).unwrap();
        assert!(r.add(ok_project("uwuwu")).is_err());
        r.add(ok_project("true-washing")).unwrap();
        assert_eq!(r.len(), 2);
    }
}
