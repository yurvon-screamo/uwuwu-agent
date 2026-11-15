//! Кроны на вики-носителе (#1058): мастер = docs/Projects/<proj>/Кроны/<имя>.md
//! (fm: schedule + agent), журнал = Кроны/<имя>/Журнал.md. Демон дописывает
//! строку запуска, ран-агент — сводку (MCP docs_append). Грамматика расписаний
//! и инварианты файра — домен (uwuwu_domain::cron).

use crate::error::UsecaseResult;
use crate::ports::{AgentSessionGateway, Clock, SessionSpec, WikiRepo};
use std::sync::Arc;
use uwuwu_domain::{jst_stamp, CronMaster, ModelRef, Schedule, UtcMinute};

/// Слэг папки кронов проекта.
pub fn crons_dir(project: &str) -> String {
    format!("Projects/{project}/Кроны")
}

pub struct CronUseCase {
    pub wiki: Arc<dyn WikiRepo>,
    pub sessions: Arc<dyn AgentSessionGateway>,
    pub clock: Arc<dyn Clock>,
    pub spawn_model: Option<ModelRef>,
    pub checkout: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FireOutcome {
    pub slug: String,
    pub session: String,
}

impl CronUseCase {
    /// Инвентарь мастеров проекта (прямые дети папки Кроны).
    pub async fn masters(&self, project: &str) -> UsecaseResult<Vec<CronMaster>> {
        let dir = crons_dir(project);
        let mut out = Vec::new();
        for (slug, md) in self.direct_children(&dir).await? {
            if let Some(m) = parse_master(&slug, &md) {
                out.push(m);
            }
        }
        Ok(out)
    }

    /// Один проход: мастера проекта, due → журнальная строка + спавн рана.
    pub async fn tick(&self, project: &str) -> UsecaseResult<Vec<FireOutcome>> {
        let mut fired = Vec::new();
        let dir = crons_dir(project);
        // list_docs даёт пути от docs/ целиком: прямые дети = мастера,
        // подпапки (журнал/ассеты) — нет
        for (slug, md) in self.direct_children(&dir).await? {
            let Some(master) = parse_master(&slug, &md) else {
                continue;
            };
            let name = slug.rsplit('/').next().unwrap_or(&slug).to_string();
            let journal = format!("{dir}/{name}/Журнал");
            let journal_md = self.wiki.read_doc(&journal).await?.unwrap_or_default();
            let now = self.clock.now_ms();
            // Инвариант DS: ран не закрылся = сигнал сбоя — последняя строка
            // журнала «запуск» без сводки старше 2 часов → warn (наблюдаемость)
            if let Some(last) = journal_md.lines().rev().find(|l| !l.trim().is_empty()) {
                if last.contains("запуск (демон)") {
                    let stale = crate::fm_lite::parse_iso_ms(
                        last.split_whitespace().nth(1).unwrap_or_default(),
                    )
                    .map(|ts| now.saturating_sub(ts) > 2 * 3_600_000)
                    .unwrap_or(false);
                    if stale {
                        tracing::warn!(event = "cron.run_stale", cron = %slug,
                            last = %last.chars().take(60).collect::<String>(),
                            "Крон-ран не закрылся >2ч — сигнал сбоя");
                    }
                }
            }
            if !master.schedule.is_due(now, last_run_ms(&journal_md)) {
                continue;
            }
            let stamp = jst_stamp(now);
            self.wiki
                .append_doc(
                    &journal,
                    &format!("- {stamp} запуск (демон)"),
                    &format!("cron: запуск {slug}"),
                )
                .await?;
            let prompt = run_prompt(project, &master, &journal_md);
            let spec = SessionSpec {
                directory: self.checkout.clone(),
                title: format!("⏰ {} · {}", master.title, stamp),
                agent: master.agent,
                model: self.spawn_model.clone(),
            };
            let session = self.sessions.create_session(&spec).await?;
            self.sessions.prompt(&session, &prompt).await?;
            tracing::info!(event = "cron.fired", cron = %slug, session = %session, "Cron fired");
            fired.push(FireOutcome { slug, session });
        }
        Ok(fired)
    }

    /// Прямые дети папки: (слэг, markdown) без захода в подпапки.
    async fn direct_children(&self, dir: &str) -> UsecaseResult<Vec<(String, String)>> {
        let prefix = format!("{dir}/");
        let mut out = Vec::new();
        for slug in self.wiki.list_docs(dir).await? {
            if !slug.starts_with(&prefix) || slug[prefix.len()..].contains('/') {
                continue;
            }
            if let Some(md) = self.wiki.read_doc(&slug).await? {
                out.push((slug, md));
            }
        }
        Ok(out)
    }
}

/// Мастер: fm-поля title/schedule/agent + тело; расписание обязано парситься.
pub fn parse_master(slug: &str, md: &str) -> Option<CronMaster> {
    let fm = crate::fm_lite::parse(md);
    let schedule = Schedule::parse(fm.get("schedule")?)?;
    Some(CronMaster {
        slug: slug.to_string(),
        title: fm.get("title").cloned().unwrap_or_else(|| slug.to_string()),
        schedule,
        agent: fm.get("agent").and_then(|a| uwuwu_domain::Agent::parse(a)),
        body: crate::fm_lite::strip(md).to_string(),
    })
}

/// Последний запуск из журнала: epoch ms строки «- <ISO-штамп> запуск …».
fn last_run_ms(journal: &str) -> Option<u64> {
    journal
        .lines()
        .rev()
        .find(|l| l.contains("запуск"))
        .and_then(|l| {
            let stamp = l.split_whitespace().nth(1)?;
            crate::fm_lite::parse_iso_ms(stamp)
        })
}

fn run_prompt(project: &str, master: &CronMaster, journal: &str) -> String {
    let tail: Vec<&str> = journal.lines().rev().take(5).collect();
    // slug мастера — полный путь от docs/: имя = хвост после последнего '/'
    let name = master.slug.rsplit('/').next().unwrap_or(&master.slug);
    format!(
        "Крон-ран «{title}» (проект {project}, расписание `{schedule}`).\n\n\
         ## Инструкция мастера\n\n{body}\n\n\
         ## Журнал (последние записи, сверху новые)\n\n{tail}\n\n\
         Выполни ран автономно. Сводку допиши в журнал:\n\
         uwuwu_docs_append {{slug: \"Projects/{project}/Кроны/{name}/Журнал\", line: \"- <итог одной строкой>\"}}.\n\
         Блокер — коммент в задачу-хвост не нужен; просто зафиксируй в журнале.",
        title = master.title,
        schedule = master.schedule.raw(),
        body = master.body,
        tail = tail.into_iter().rev().collect::<Vec<_>>().join("\n"),
    )
}

/// Минуты UTC для планировщика (переиспользуем доменный UtcMinute через Clock).
pub fn utc_minute_of(clock: &dyn Clock) -> UtcMinute {
    clock.utc_minute()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn master_requires_valid_schedule() {
        let md = "---\ntitle: Тест\nschedule: 5m\n---\n\nТело";
        let m = parse_master("Projects/uwuwu/Кроны/x", md).unwrap();
        assert_eq!(m.schedule.raw(), "5m");
        assert_eq!(m.title, "Тест");

        let bad = "---\ntitle: Битый\nschedule: не-расписание\n---\n\nТело";
        assert!(
            parse_master("x", bad).is_none(),
            "битое расписание — не мастер"
        );

        let nofm = "просто текст без fm";
        assert!(parse_master("x", nofm).is_none());
    }

    #[test]
    fn last_run_reads_iso_stamp() {
        let journal = "- мусор\n- 2026-11-15T02:29:00+09:00 запуск (демон)\n- 2026-11-15T03:00:00+09:00 сводка";
        assert_eq!(
            last_run_ms(journal),
            crate::fm_lite::parse_iso_ms("2026-11-15T02:29:00+09:00")
        );
        assert_eq!(last_run_ms("пусто"), None);
    }
}
