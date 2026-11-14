//! Конфиг uwuwu-server: TOML-файл (политики/проекты) + env-секреты.
//! Нативные ошибки загрузки. Порт из TOML-структур в TickPlan — внизу.

use serde::Deserialize;
use std::path::PathBuf;
use uwuwu_domain::{PeakWindow, SpawnPolicy};
use uwuwu_usecase::{BoardPlan, Mode, TickPlan};

pub const ENV_CONFIG: &str = "UWUWU_SERVER_CONFIG";
pub const ENV_GH_TOKEN: &str = "GH_TOKEN";
pub const ENV_OPENCODE_PASSWORD: &str = "UWUWU_OPENCODE_PASSWORD";
pub const ENV_BEARER: &str = "UWUWU_SERVER_BEARER";
pub const ENV_STATE_PATH: &str = "UWUWU_SERVER_STATE";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModeCfg {
    Observe,
    Live,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HubCfg {
    pub repo: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SpawnModelCfg {
    pub provider: String,
    pub id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExecutorCfg {
    pub name: String,
    pub url: String,
    pub spawn_model: Option<SpawnModelCfg>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct PeakCfg {
    pub enabled: bool,
    pub start_min: u32,
    pub end_min: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PolicyCfg {
    pub cup: usize,
    pub peak: PeakCfg,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProjectCfg {
    pub name: String,
    pub board: String,
    pub repo: String,
    /// Мастер-чекаут проекта на исполнителе (cwd bootstrap-сессии).
    pub checkout: String,
    /// Корень env задач на исполнителе (~/agents-env).
    pub env_root: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub mode: ModeCfg,
    #[serde(default = "default_poll")]
    pub poll_sec: u64,
    #[serde(default = "default_port")]
    port_cfg: u16,
    pub hub: HubCfg,
    pub executor: ExecutorCfg,
    pub policy: PolicyCfg,
    pub project: Vec<ProjectCfg>,
}

fn default_poll() -> u64 {
    180
}

fn default_port() -> u16 {
    8090
}

/// Секреты из env: отсутствующий секрет — честная ошибка старта.
#[derive(Debug, Clone)]
pub struct Secrets {
    pub gh_token: String,
    pub opencode_password: Option<String>,
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
    #[error("конфиг: {0}")]
    Validation(String),
}

impl Config {
    /// Порт: env PORT (Railway) > TOML port > дефолт.
    pub fn port(&self) -> u16 {
        std::env::var("PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(self.port_cfg)
    }

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
        if !self.executor.url.starts_with("http") {
            return Err(ConfigError::Validation("executor.url не http(s)".into()));
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

    /// Секреты: observe обходится без пароля опенкода (спавнов нет).
    pub fn secrets(&self) -> Result<Secrets, ConfigError> {
        let gh_token = non_empty_env(ENV_GH_TOKEN).ok_or_else(|| {
            ConfigError::Validation(format!("{ENV_GH_TOKEN} обязателен (доступ к hub)"))
        })?;
        let bearer = non_empty_env(ENV_BEARER).ok_or_else(|| {
            ConfigError::Validation(format!("{ENV_BEARER} обязателен (HTTP/MCP гейт)"))
        })?;
        let opencode_password = non_empty_env(ENV_OPENCODE_PASSWORD);
        if self.mode == ModeCfg::Live && opencode_password.is_none() {
            return Err(ConfigError::Validation(format!(
                "{ENV_OPENCODE_PASSWORD} обязателен в live-режиме (спавны)"
            )));
        }
        Ok(Secrets {
            gh_token,
            opencode_password,
            bearer,
        })
    }

    /// Путь state-файла: env > рядом с конфигом.
    pub fn state_path(&self, config_path: &std::path::Path) -> PathBuf {
        std::env::var(ENV_STATE_PATH)
            .map(PathBuf::from)
            .unwrap_or_else(|_| config_path.with_file_name("state.json"))
    }

    /// Порт конфига → план тика юзкейса.
    pub fn to_tick_plan(&self) -> TickPlan {
        TickPlan {
            mode: match self.mode {
                ModeCfg::Observe => Mode::Observe,
                ModeCfg::Live => Mode::Live,
            },
            policy: SpawnPolicy {
                cup: self.policy.cup,
                peak: PeakWindow {
                    enabled: self.policy.peak.enabled,
                    start_min: self.policy.peak.start_min,
                    end_min: self.policy.peak.end_min,
                },
            },
            boards: self
                .project
                .iter()
                .map(|p| BoardPlan {
                    project: p.name.clone(),
                    board: p.board.clone(),
                    repo: p.repo.clone(),
                    checkout: p.checkout.clone(),
                    env_root: p.env_root.clone(),
                })
                .collect(),
        }
    }
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example() -> &'static str {
        include_str!("../../../config.example.toml")
    }

    #[test]
    fn parses_example_into_tick_plan() {
        let cfg = toml::from_str::<Config>(example()).expect("example валиден");
        assert_eq!(cfg.mode, ModeCfg::Observe);
        assert_eq!(cfg.project.len(), 1);
        assert!(cfg.policy.peak.enabled);
        let plan = cfg.to_tick_plan();
        assert_eq!(plan.boards[0].board, "Задачи uwuwu");
        assert_eq!(plan.policy.cup, 7);
        assert_eq!(plan.mode, Mode::Observe);
    }

    #[test]
    fn rejects_empty_projects() {
        let raw = r#"
            mode = "observe"
            [hub]
            repo = "a/b"
            [executor]
            name = "kisa"
            url = "http://127.0.0.1:1"
            [policy]
            cup = 7
            peak = { enabled = false, start_min = 0, end_min = 0 }
        "#;
        let err = toml::from_str::<Config>(raw).expect_err("должен упасть");
        assert!(err.to_string().contains("project"), "{err}");
    }
}
