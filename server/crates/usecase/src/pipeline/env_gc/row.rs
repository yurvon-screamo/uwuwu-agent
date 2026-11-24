//! Исполнение одной строки env-gc (#1431): гарды сессий, команда
//! сноса, вердикт-грамматика «ENV-GC <row> <verb>» и её парсинг.

use super::{EnvGcOutcome, EnvGcPolicy, ENV_GC_VERDICT_DEADLINE_MS};
use crate::router::Gateway;
use uwuwu_domain::{sighting_is_live, SessionSighting, Task};

/// Контекст строки прохода: общее для всех целей доски, не дублируется
/// аргументами (clippy too_many_arguments — 8/7).
pub(super) struct RowCtx<'a> {
    pub board_name: &'a str,
    pub env_root: &'a str,
    pub bootstrap: &'a str,
    pub sessions: &'a Gateway,
    /// Носитель доски: свежая проба статуса перед диспатчем (TOCTOU
    /// ревайва #1431 — снапшот прохода устарает на длинном батче).
    pub board: &'a dyn crate::ports::TaskBoardRepo,
    pub sightings: &'a [SessionSighting],
    pub policy: EnvGcPolicy,
}

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

/// Снос одной строки-цели: ревалидация статуса → гарды сессий → команда → вердикт.
pub(super) async fn sweep_row(ctx: &RowCtx<'_>, task: &Task, out: &mut EnvGcOutcome) {
    let number = task.number();
    // Ревалидация перед диспатчем (TOCTOU ревайва #1431): снапшот прохода
    // устарает на длинном батче — возрождённая владельцем строка
    // (fast-track Done → In Progress) отсекается свежей пробой носителя.
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
    if env_sessions
        .iter()
        .any(|s| sighting_is_live(s, ctx.policy.now_ms))
    {
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
        .filter(|s| !sighting_is_live(s, ctx.policy.now_ms))
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
    let row = number.to_string();
    // args = только row: opencode резолвит шаблон команды, подставляя в
    // $1 только первый токен args (argsRegex в session/prompt.ts), а
    // шаблон task-env-gc.md несёт слово sweep литерально — «sweep <row>»
    // в args делали $1 = "sweep", скрипт вердиктил чужой env
    // «ENV-GC sweep ABSENT» и вердикт никогда не совпадал с маркером
    // сервера (bug #1506, LIVE 22.11 — ретрай-цикл на Done-строках).
    // Контракт единый с task-env: args = row, подкоманда живёт в шаблоне.
    if let Err(e) = ctx
        .sessions
        .run_command(ctx.bootstrap, "task-env-gc", &row)
        .await
    {
        out.failed += 1;
        tracing::warn!(
            event = "env.gc_fail",
            board = ctx.board_name,
            number,
            error = %e,
            "Команда сноса не принята исполнителем"
        );
        return;
    }
    out.dispatched += 1;
    // Хвостовой пробел в маркере: «ENV-GC 143» ложно содержится в чужом
    // вердикте «ENV-GC 1431 SWEPT» (префиксные row-id одного прохода) —
    // скрипт всегда печатает verb после row, пробел гарантирует грань.
    let marker = format!("ENV-GC {row} ");
    let verdict = ctx
        .sessions
        .wait_user_message(ctx.bootstrap, &marker, ENV_GC_VERDICT_DEADLINE_MS)
        .await
        .ok()
        .flatten()
        .and_then(|text| parse_verdict(&text, &row));
    match verdict {
        Some(Verdict::Swept) => {
            out.swept += 1;
            tracing::info!(
                event = "env.swept",
                board = ctx.board_name,
                number,
                "env снесён"
            );
        }
        Some(Verdict::Dirty) => {
            out.dirty += 1;
            tracing::info!(
                event = "env.dirty",
                board = ctx.board_name,
                number,
                "env грязный — не снесён, разбор владельца по логам"
            );
        }
        Some(Verdict::NoRepo) => {
            out.norepo += 1;
            tracing::info!(
                event = "env.norepo",
                board = ctx.board_name,
                number,
                "env без repo — не снесён, ручной разбор"
            );
        }
        Some(Verdict::Foreign) => {
            out.foreign += 1;
            tracing::info!(
                event = "env.foreign",
                board = ctx.board_name,
                number,
                "env не принадлежит чекауту проекта — не снесён"
            );
        }
        Some(Verdict::Absent) => {
            out.absent += 1;
            tracing::info!(
                event = "env.absent",
                board = ctx.board_name,
                number,
                "env уже отсутствует"
            );
        }
        Some(Verdict::Fail) | None => {
            out.failed += 1;
            tracing::warn!(
                event = "env.gc_fail",
                board = ctx.board_name,
                number,
                "Снос не доведён (FAIL или дедлайн вердикта) — переедет следующим проходом"
            );
        }
    }
}
