//! Исполнение прохода env-gc (#1431, батч #1526): гарды сессий,
//! перепроба носителя перед диспатчем, сбор вердикт-грамматики
//! «ENV-GC <row> <verb>» из батч-сообщения ленты.

use std::time::Instant;

use super::{EnvGcOutcome, VERDICT_READ_GRACE_MS, VERDICT_READ_MAX_RETRIES};
use crate::router::Gateway;
use uwuwu_domain::{sighting_is_live, SessionSighting, Task};

/// Вердикт скрипта (грамматика «ENV-GC <row> <verb>»).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Swept,
    Dirty,
    NoRepo,
    Foreign,
    Absent,
    Fail,
}

impl Verdict {
    fn parse(verb: &str) -> Option<Self> {
        match verb {
            "SWEPT" => Some(Self::Swept),
            "DIRTY" => Some(Self::Dirty),
            "NOREPO" => Some(Self::NoRepo),
            "FOREIGN" => Some(Self::Foreign),
            "ABSENT" => Some(Self::Absent),
            "FAIL" => Some(Self::Fail),
            _ => None,
        }
    }
}

/// Вердикт строки в тексте user-сообщения (строка «ENV-GC <row> <verb>»).
/// Точный матч по токенам — «ENV-GC 143» не матчится в чужом
/// «ENV-GC 1431 SWEPT» (префиксные row-id одного батча).
fn parse_verdict(text: &str, row: &str) -> Option<Verdict> {
    text.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        match (parts.next(), parts.next(), parts.next()) {
            (Some("ENV-GC"), Some(r), Some(verb)) if r == row => Verdict::parse(verb),
            _ => None,
        }
    })
}

/// Слепки сессий в env строки: точные пути env-корня и repo (сессии
/// создаются в корне, агенты живут и в repo — семантика точного
/// равенства guard.rs, без нормализаций).
fn sessions_in_env<'a>(
    sightings: &'a [SessionSighting],
    env_root: &str,
    number: u64,
) -> Vec<&'a SessionSighting> {
    let root = format!("{env_root}/{number}");
    let repo = format!("{root}/repo");
    sightings
        .iter()
        .filter(|s| s.directory == root || s.directory == repo)
        .collect()
}

/// Контекст pre-flight строки: общее для всех целей доски, не
/// дублируется аргументами (clippy too_many_arguments).
pub(super) struct PreflightCtx<'a> {
    pub board: &'a dyn crate::ports::TaskBoardRepo,
    pub sessions: &'a Gateway,
    pub board_name: &'a str,
    pub env_root: &'a str,
    pub sightings: &'a [SessionSighting],
    pub now_ms: u64,
}

/// Pre-flight строки без команд: ревалидация статуса носителя (TOCTOU
/// ревайва #1431) → гарды сессий в env → гашение завершённых сессий.
/// Готовая строка — в батч диспатча; пропущенная — в счётчики.
pub(super) async fn preflight_row<'a>(
    ctx: &PreflightCtx<'a>,
    task: &'a Task,
    out: &mut EnvGcOutcome,
    ready: &mut Vec<&'a Task>,
) {
    let number = task.number();
    // Ревалидация перед диспатчем (TOCTOU ревайва #1431): возрождённая
    // владельцем строка (fast-track Done → In Progress) отсекается.
    // Проба недоступна — fail-safe skip («не знаю» ≠ «можно сносить»).
    match ctx.board.row_status(ctx.board_name, number).await {
        Ok(Some(st)) if !st.is_terminal() => {
            out.revoked += 1;
            tracing::info!(
                event = "env.revoked",
                board = ctx.board_name,
                number,
                "строка возрождена после снапшота — снос отменён"
            );
            return;
        }
        Ok(_) => {}
        Err(e) => {
            out.failed += 1;
            tracing::warn!(
                event = "env.gc_fail",
                board = ctx.board_name,
                number,
                error = %e,
                "Проба статуса недоступна — строка переедет следующим проходом"
            );
            return;
        }
    }
    let env_sessions = sessions_in_env(ctx.sightings, ctx.env_root, number);
    // Гард 2: живая сессия в env — строка переедет следующим проходом.
    if env_sessions.iter().any(|s| sighting_is_live(s, ctx.now_ms)) {
        out.held_live += 1;
        tracing::info!(
            event = "env.gc.held",
            board = ctx.board_name,
            number,
            "живая сессия в env — снос отложен"
        );
        return;
    }
    // Гард 3: завершённые сессии, числящиеся в env, гасятся перед сносом
    // (ADR-041 «терминируются сессии»; 404 — идемпотентно ок).
    for s in env_sessions
        .iter()
        .filter(|s| !sighting_is_live(s, ctx.now_ms))
    {
        if let Err(e) = ctx.sessions.delete_session(&s.id).await {
            tracing::warn!(
                event = "env.session_delete_failed",
                board = ctx.board_name,
                number,
                error = %e,
                "Сессия не погашена — снос продолжается (сессия с финалом)"
            );
        }
    }
    ready.push(task);
}

/// Перепроба статуса готовой строки непосредственно перед диспатчем
/// (#1526, ревью раунд 3): pre-flight — N последовательных проб, stalled
/// проба устаревает ранние строки; перепроба сужает окно «проба → снос»
/// до длительности последнего цикла. Живое чтение носителя — кеш не
/// выдаёт своё же старое. Возрождённая строка выбывает из батча.
pub(super) async fn revalidate_row<'a>(
    board: &dyn crate::ports::TaskBoardRepo,
    board_name: &str,
    task: &'a Task,
    out: &mut EnvGcOutcome,
    batch: &mut Vec<&'a Task>,
) {
    let number = task.number();
    match board.row_status(board_name, number).await {
        Ok(Some(st)) if st.is_terminal() => batch.push(task),
        Ok(Some(_)) => {
            out.revoked += 1;
            tracing::info!(
                event = "env.revoked",
                board = board_name,
                number,
                "строка возрождена между pre-flight и диспатчем — из батча"
            );
        }
        Ok(None) => batch.push(task),
        Err(e) => {
            out.failed += 1;
            tracing::warn!(
                event = "env.gc_fail",
                board = board_name,
                number,
                error = %e,
                "Перепроба статуса недоступна — строка переедет следующим проходом"
            );
        }
    }
}

/// Сбор вердиктов батча: одно wait_user_message по маркеру-префиксу
/// «ENV-GC » (матчится любой вердикт-строкой) со страх-дедлайном, затем
/// parse-all по готовым строкам. POST /command синхронен — на Ok команды
/// сообщение уже в ленте; три исхода ожидания (пин Design #1526):
/// Ok(Some) → parse-all (недостающие — failed); Ok(None) → все failed;
/// Err (опрос упал транспортом) → ретрай до страх-дедлайна, исчерпан —
/// все failed + env.verdict_read_failed (транспортный отказ чтения не
/// подмешивается в «снос не доведён» — класс #1506).
pub(super) async fn collect_verdicts(
    sessions: &Gateway,
    bootstrap: &str,
    targets: &[&Task],
    out: &mut EnvGcOutcome,
) {
    let Some(text) = wait_feed(sessions, bootstrap).await else {
        out.failed += targets.len();
        return;
    };
    for t in targets {
        let row = t.number().to_string();
        match parse_verdict(&text, &row) {
            Some(Verdict::Swept) => {
                out.swept += 1;
                tracing::info!(event = "env.swept", number = t.number(), "env снесён");
            }
            Some(Verdict::Dirty) => {
                out.dirty += 1;
                tracing::info!(
                    event = "env.dirty",
                    number = t.number(),
                    "env грязный — не снесён, разбор владельца по логам"
                );
            }
            Some(Verdict::NoRepo) => {
                out.norepo += 1;
                tracing::info!(
                    event = "env.norepo",
                    number = t.number(),
                    "env без repo — не снесён, ручной разбор"
                );
            }
            Some(Verdict::Foreign) => {
                out.foreign += 1;
                tracing::info!(
                    event = "env.foreign",
                    number = t.number(),
                    "env не принадлежит чекауту проекта — не снесён"
                );
            }
            Some(Verdict::Absent) => {
                out.absent += 1;
                tracing::info!(
                    event = "env.absent",
                    number = t.number(),
                    "env уже отсутствует"
                );
            }
            Some(Verdict::Fail) | None => {
                out.failed += 1;
                tracing::warn!(
                    event = "env.gc_fail",
                    number = t.number(),
                    "Снос не доведён (FAIL или вердикта нет в батч-сообщении) — переедет следующим проходом"
                );
            }
        }
    }
}

/// Ожидание батч-сообщения: wait_user_message с маркером «ENV-GC » и
/// страх-дедлайном; Err опроса (адаптер возвращает его немедленно) —
/// ретрай до страх-дедлайна и/или капа попыток. Пауза между ретраями в
/// юзкейсе недоступна (слой зависит только от домена, tokio — dev-dep):
/// ограничителем служит кап попыток, при живом носителе первая же
/// попытка возвращает ленту. None — страх-дедлайн исчерпан (Ok(None)
/// или Err-исчерпание).
async fn wait_feed(sessions: &Gateway, bootstrap: &str) -> Option<String> {
    let started = Instant::now();
    let mut retries = 0usize;
    loop {
        let remaining = VERDICT_READ_GRACE_MS.saturating_sub(started.elapsed().as_millis() as u64);
        if remaining == 0 || retries > VERDICT_READ_MAX_RETRIES {
            tracing::warn!(
                event = "env.verdict_read_failed",
                retries,
                "Лента батч-команды не прочитана (страх-дедлайн/кап ретраев) — вердикты не собраны"
            );
            return None;
        }
        match sessions
            .wait_user_message(bootstrap, "ENV-GC ", remaining)
            .await
        {
            Ok(found) => return found,
            Err(e) => {
                retries += 1;
                tracing::warn!(
                    event = "env.verdict_read_retry",
                    retry = retries,
                    error = %e,
                    "Опрос ленты упал — ретрай до страх-дедлайна/капа"
                );
            }
        }
    }
}
