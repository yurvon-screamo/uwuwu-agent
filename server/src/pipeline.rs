//! Тик конвейера: чтение досок → маршрутизация → гейты (CUP, пик) → действия.
//! Срез 1: решения логируются структурно; observe-режим не мутирует ничего,
//! live-режим готовит спавны (исполнение — срез 1b, пока актёр-заглушка).

use crate::clock::{in_peak, Clock};
use crate::config::{Config, ProjectCfg};
use crate::domain::{Decision, Priority, TaskRow};
use crate::hub::BoardReader;
use serde::Serialize;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Default)]
pub struct TickSummary {
    pub rows_total: usize,
    pub spawns_allowed: usize,
    pub spawns_held_cup: usize,
    pub spawns_deferred_peak: usize,
    pub observe: usize,
}

/// Один тик по всем проектам конфига.
pub async fn tick(
    cfg: &Config,
    hub: &dyn BoardReader,
    clock: &dyn Clock,
) -> anyhow::Result<TickSummary> {
    let mut summary = TickSummary::default();
    let peak_now = in_peak(clock.utc_minute(), &cfg.policy.peak);
    for project in &cfg.project {
        let rows = hub.read_board(&project.board).await?;
        summary.rows_total += rows.len();
        tick_project(project, &rows, cfg, peak_now, &mut summary).await;
    }
    tracing::info!(
        event = "tick.completed",
        rows = summary.rows_total,
        spawns = summary.spawns_allowed,
        held_cup = summary.spawns_held_cup,
        deferred_peak = summary.spawns_deferred_peak,
        mode = ?cfg.mode,
        "Tick completed"
    );
    Ok(summary)
}

async fn tick_project(
    project: &ProjectCfg,
    rows: &[TaskRow],
    cfg: &Config,
    peak_now: bool,
    summary: &mut TickSummary,
) {
    // Активные = LLM-тратящие статусы; спавн строки не меняет счётчик —
    // строка уже активна своим статусом. Гейт CUP держит спавны сверх капа.
    let active = rows.iter().filter(|r| r.status.is_active()).count();
    let mut queued: Vec<&TaskRow> = rows.iter().collect();
    queued.sort_by_key(|r| r.priority.rank());
    for row in queued {
        let decision = row.route();
        log_decision(project, row, &decision);
        match decision {
            Decision::Spawn {
                number,
                slot,
                reason,
            } => {
                if peak_now && row.priority != Priority::High {
                    summary.spawns_deferred_peak += 1;
                    tracing::info!(
                        event = "spawn.deferred_peak",
                        project = %project.name,
                        number,
                        priority = ?row.priority,
                        "Spawn deferred: peak window"
                    );
                    continue;
                }
                if active >= cfg.policy.cup {
                    summary.spawns_held_cup += 1;
                    tracing::info!(
                        event = "spawn.held_cup",
                        project = %project.name,
                        number,
                        active,
                        cap = cfg.policy.cup,
                        "Spawn held: CUP cap"
                    );
                    continue;
                }
                summary.spawns_allowed += 1;
                tracing::info!(
                    event = "spawn.allowed",
                    project = %project.name,
                    number,
                    slot = ?slot,
                    reason = %reason,
                    mode = ?cfg.mode,
                    "Spawn allowed"
                );
            }
            Decision::AutodoneProbe { number } => {
                tracing::info!(
                    event = "autodone.probe",
                    project = %project.name,
                    number,
                    pr = row.pr.as_deref().unwrap_or(""),
                    "Autodone probe (срез 1b)"
                );
            }
            Decision::Observe { .. } => {
                summary.observe += 1;
            }
        }
    }
}

fn log_decision(project: &ProjectCfg, row: &TaskRow, decision: &Decision) {
    tracing::debug!(
        event = "route.decision",
        project = %project.name,
        number = row.number,
        status = ?row.status,
        priority = ?row.priority,
        decision = ?decision,
        "Route decision"
    );
}

/// Shared-состояние для /health.
pub type SharedTick = Arc<std::sync::RwLock<TickSummary>>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::UtcMinute;
    use async_trait::async_trait;

    struct FakeHub(Vec<TaskRow>);
    #[async_trait]
    impl BoardReader for FakeHub {
        async fn read_board(&self, _board: &str) -> anyhow::Result<Vec<TaskRow>> {
            Ok(self.0.clone())
        }
    }

    struct FixedClock(u32, u32);
    impl Clock for FixedClock {
        fn utc_minute(&self) -> UtcMinute {
            UtcMinute {
                minute_of_day: self.0,
                weekday: self.1,
            }
        }
    }

    fn cfg(cup: usize) -> Config {
        let raw = format!(
            r#"
            mode = "observe"
            [hub]
            repo = "a/b"
            [executor]
            name = "kisa"
            url = "http://127.0.0.1:1"
            [policy]
            cup = {cup}
            peak = {{ enabled = false, start_min = 0, end_min = 0 }}
            [[project]]
            name = "p"
            board = "B"
            repo = "a/c"
            checkout = "/tmp/x"
            "#
        );
        toml::from_str(&raw).unwrap()
    }

    fn row(n: u64, status: &str) -> TaskRow {
        TaskRow::from_parts(n, format!("t{n}"), status, "---\n---\nx")
    }

    #[tokio::test]
    async fn cup_holds_spawns_over_budget() {
        let mut rows: Vec<TaskRow> = (1..=7).map(|n| row(n, "In Progress")).collect();
        rows.push(row(8, "Ревью плана"));
        let hub = FakeHub(rows);
        let out = tick(&cfg(7), &hub, &FixedClock(100, 3)).await.unwrap();
        // активных 8 > капа 7: всё под гейтом — конвейер не берёт сверх капа
        assert_eq!(out.rows_total, 8);
        assert_eq!(out.spawns_allowed, 0);
        assert_eq!(out.spawns_held_cup, 8);
    }

    #[tokio::test]
    async fn within_cap_allows_all() {
        let rows = vec![row(1, "Design"), row(2, "In Progress"), row(3, "Todo")];
        let hub = FakeHub(rows);
        let out = tick(&cfg(7), &hub, &FixedClock(100, 3)).await.unwrap();
        assert_eq!(out.spawns_allowed, 2, "Design + In Progress");
        assert_eq!(out.observe, 1, "Todo — наблюдение");
    }

    #[tokio::test]
    async fn peak_defers_non_high_but_not_high_priority() {
        let mut rows = vec![row(1, "Design"), row(2, "In Progress")];
        rows.push(TaskRow::from_parts(
            3,
            "t3".into(),
            "Design",
            "---\npriority: high\n---\nx",
        ));
        let mut c = cfg(7);
        c.policy.peak = crate::config::Peak {
            enabled: true,
            start_min: 360,
            end_min: 600,
        };
        let hub = FakeHub(rows);
        let out = tick(&c, &hub, &FixedClock(540, 3)).await.unwrap();
        assert_eq!(out.spawns_deferred_peak, 2);
        assert_eq!(out.spawns_allowed, 1, "high работает в пик");
    }

    #[tokio::test]
    async fn claimed_session_observes() {
        let rows = vec![TaskRow::from_parts(
            1,
            "t".into(),
            "In Progress",
            "---\ndev-session: ses_1\n---\nx",
        )];
        let hub = FakeHub(rows);
        let out = tick(&cfg(7), &hub, &FixedClock(100, 3)).await.unwrap();
        assert_eq!(out.spawns_allowed, 0);
        assert_eq!(out.observe, 1, "живая заявка — спавна нет");
    }
}
