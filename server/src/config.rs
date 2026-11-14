//! Конфиг uwuwu-server: TOML-файл (политики/проекты) + env-секреты.
//! Секреты (GH_TOKEN, basic-пароль опенкода, bearer) живут в среде —
//! в Railway env, не в репо.

use serde::Deserialize;
use std::path::PathBuf;

pub const ENV_CONFIG: &str = "UWUWU_SERVER_CONFIG";
pub const ENV_GH_TOKEN: &str = "GH_TOKEN";
pub const ENV_BEARER: &str = "UWUWU_SERVER_BEARER";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Тик читает доски и логирует решения; мутаций и спавнов нет.
    Observe,
    /// Полный конвейер.
    Live,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HubCfg {
    pub repo: String,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Peak {
    pub enabled: bool,
    /// Минуты от полуночи UTC, включительно.
    pub start_min: u32,
    pub end_min: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Policy {
    pub cup: usize,
    pub peak: Peak,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProjectCfg {
    pub name: String,
    pub board: String,
    pub repo: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub mode: Mode,
    #[serde(default = "default_poll")]
    pub poll_sec: u64,
    #[serde(default = "default_port")]
    port_cfg: u16,
    pub hub: HubCfg,
    pub policy: Policy,
    pub project: Vec<ProjectCfg>,
}

impl Config {
    /// Порт: env PORT (Railway) > TOML port > дефолт.
    pub fn port(&self) -> u16 {
        std::env::var("PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(self.port_cfg)
    }
}

fn default_poll() -> u64 {
    180
}

fn default_port() -> u16 {
    8090
}

/// Секреты, собранные из env: отсутствующий секрет — честная ошибка
/// старта (fail-loud), а не тихая деградация конвейера.
#[derive(Debug, Clone)]
pub struct Secrets {
    pub gh_token: String,
    pub bearer: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("чтение конфига {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("парсинг конфига {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("валидация конфига: {0}")]
    Validation(String),
}

impl Config {
    pub fn load(path: &std::path::Path) -> Result<Self, ConfigError> {
        let raw = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let cfg: Config = toml::from_str(&raw).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.poll_sec < 10 || self.poll_sec > 3600 {
            return Err(ConfigError::Validation("poll_sec вне 10..=3600".into()));
        }
        if self.project.is_empty() {
            return Err(ConfigError::Validation("нет ни одного [[project]]".into()));
        }
        if self.policy.cup == 0 {
            return Err(ConfigError::Validation("cup = 0 (конвейер стоит)".into()));
        }
        for p in &self.project {
            if p.name.is_empty() || p.board.is_empty() || p.repo.is_empty() {
                return Err(ConfigError::Validation(format!(
                    "проект {}{{name,board,repo}} обязан быть непустым",
                    p.name
                )));
            }
        }
        Ok(())
    }

    /// Секреты из env. Observe-режиму basic-пароль опенкода не нужен
    /// (спавнов нет) — остальные секреты обязательны всегда.
    pub fn secrets(&self) -> Result<Secrets, ConfigError> {
        let gh_token = std::env::var(ENV_GH_TOKEN).ok().filter(|v| !v.is_empty());
        let gh_token = gh_token.ok_or_else(|| {
            ConfigError::Validation(format!("{ENV_GH_TOKEN} обязателен (доступ к hub)"))
        })?;
        let bearer = std::env::var(ENV_BEARER).ok().filter(|v| !v.is_empty());
        let bearer = bearer.ok_or_else(|| {
            ConfigError::Validation(format!("{ENV_BEARER} обязателен (MCP/health гейт)"))
        })?;
        Ok(Secrets { gh_token, bearer })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example() -> &'static str {
        include_str!("../config.example.toml")
    }

    #[test]
    fn parses_example() {
        let cfg = toml::from_str::<Config>(example()).expect("example валиден");
        assert_eq!(cfg.mode, Mode::Observe);
        assert_eq!(cfg.project.len(), 1);
        assert_eq!(cfg.project[0].board, "Задачи uwuwu");
        assert_eq!(cfg.policy.cup, 7);
        assert!(cfg.policy.peak.enabled);
    }

    #[test]
    fn rejects_empty_projects() {
        let raw = r#"
            mode = "observe"
            [hub]
            repo = "a/b"
            [policy]
            cup = 7
            peak = { enabled = false, start_min = 0, end_min = 0 }
        "#;
        let err = toml::from_str::<Config>(raw).expect_err("должен упасть");
        assert!(err.to_string().contains("project"), "{err}");
    }
}
