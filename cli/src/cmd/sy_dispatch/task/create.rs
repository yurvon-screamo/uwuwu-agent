//! task create: валидация флагов до записей, дедуп тайтла, uploads до мутаций,
//! create + крепления schedule/agent/attach-refs, контракт ответов (ADR-067).

use crate::cmd::cli::TaskSub;
use crate::siyuan::SiYuan;
use crate::siyuan_task as syt;
use anyhow::{Context, Result};

use super::helpers::{
    created_header, print_final_body, reload_task, task_header_md, warn_unreadable_state,
};
use crate::cmd::sy_dispatch::attach::{attach_refs_md, validate_attach_files};
use crate::cmd::sy_dispatch::guards::read_stdin;
use super::create_sync::{sync_agent_on_create, sync_schedule_on_create};

pub(super) async fn run(sy: &SiYuan, project: &str, command: TaskSub) -> Result<()> {
    let TaskSub::Create {
        title,
        r#type: ty,
        priority,
        autodone,
        stdin,
        attach,
        schedule,
        agent,
        status,
        ..
    } = command
    else {
        unreachable!("task_run роутит Create в create::run");
    };
    let attach_files = validate_attach_files(&attach)?;
    let ctx = if stdin { read_stdin()? } else { String::new() };
    // канон-формы до любых записей: порядок валидаций дословно из монолита
    // (ty → priority → status → schedule → agent)
    let (ty_canonical, priority_canonical, initial_status) = validate_create_flags(
        ty.as_deref(),
        priority.as_deref(),
        status.as_deref(),
        schedule.as_deref(),
        agent.as_deref(),
    )?;
    // пред-чек дедупа тайтла ДО uploads: ретрай с живой строкой не
    // должен грузить ассеты впустую (внутренний чек create остаётся
    // авторитетным — гонка между чеками ловится им же)
    precheck_duplicate_title(sy, project, &title, attach_files).await?;
    // upload всех до мутаций доски: сбой не оставляет задачу без скрина;
    // мусор-ассеты возможен только при падении между upload и create
    let mut uploaded = Vec::new();
    for file in attach_files {
        uploaded.push(sy.upload_asset(file).await?);
    }
    let (item, body) = syt::create(
        sy,
        project,
        &title,
        &ctx,
        initial_status,
        &syt::CreateProps {
            ty: Some(&ty_canonical),
            priority: priority_canonical.as_deref(),
            autodone,
        },
    )
    .await?;
    if let Some(expr) = schedule.as_deref() {
        sync_schedule_on_create(sy, project, &item, &body, expr).await?;
    }
    if let Some(name) = agent.as_deref() {
        sync_agent_on_create(sy, &item, &body, name).await?;
    }
    if !uploaded.is_empty() {
        let refs = attach_refs_md(&uploaded);
        sy.append_block_md(&body, &refs).await.with_context(|| {
            format!(
                "задача создана (строка {item}, тело {body}), но референсы не дописаны — \
                         доложи: task body {item} --attach <файлы>"
            )
        })?;
    }
    // контракт ответов: ответ = созданная задача целиком, перечитанная
    // с ядра (шапка перечитанной строки + финальный экспорт тела).
    // Перечитывание/экспорт сорвались → ADR-067: rc 0 + warning +
    // рецепт, ретраить create нельзя (строка уже существует). В окне
    // срыва stdout всё равно несёт «Строка: <id>» — шапка собирается
    // из факта create (item/title/тип/Draft известны без render;
    // лаг render — документированная реальность ядра), иначе
    // потребители stdout (крон-ран-карточки) теряли бы id.
    match syt::find_board(sy, project).await {
        Ok(board) => match reload_task(sy, &board, &item).await {
            Ok(fresh) => match task_header_md(sy, &board, &fresh).await {
                Ok(header) => {
                    print!("{header}");
                    print_final_body(sy, &body, "тело").await;
                }
                Err(err) => warn_unreadable_state(&item, &err),
            },
            Err(err) => {
                print!(
                    "{}",
                    created_header(&board, &item, &title, &ty_canonical, initial_status)
                );
                warn_unreadable_state(&item, &err);
            }
        },
        Err(err) => warn_unreadable_state(&item, &err),
    }
    if !uploaded.is_empty() {
        let paths: Vec<&str> = uploaded.iter().map(|a| a.asset_path.as_str()).collect();
        println!("  ассеты: {}", paths.join(", "));
    }
    Ok(())
}

/// Пред-чек дедупа тайтла ДО uploads: ретрай с живой строкой не должен
/// грузить ассеты впустую (внутренний чек create остаётся авторитетным —
/// гонка между чеками ловится им же). Без аттачей — пропускается.
async fn precheck_duplicate_title(
    sy: &SiYuan,
    project: &str,
    title: &str,
    attach_files: &[std::path::PathBuf],
) -> Result<()> {
    if attach_files.is_empty() {
        return Ok(());
    }
    let board = syt::find_board(sy, project).await?;
    let rows = syt::read_rows(sy, &board).await?;
    let canon = syt::normalize_title(title);
    if rows.iter().any(|r| syt::normalize_title(&r.title) == canon) {
        anyhow::bail!("живая строка с тайтлом «{title}» уже есть на доске — дубль");
    }
    if !syt::body_doc_by_title(sy, &board, title).await?.is_empty() {
        anyhow::bail!("тело задачи «{title}» уже есть");
    }
    Ok(())
}

/// Валидация флагов create до любых записей: тип (пустой бессмысленен,
/// дефолт — DEFAULT_TASK_TYPE), статус (строго машинный Todo), расписание и
/// агент (пустые бессмысленны, грамматики валидируются). Возвращает канон.
fn validate_create_flags(
    ty: Option<&str>,
    priority: Option<&str>,
    status: Option<&str>,
    schedule: Option<&str>,
    agent: Option<&str>,
) -> Result<(String, Option<String>, &'static str)> {
    // тип валидируется до любых записей: пустой на create
    // бессмысленен (паритет расписания), канон — дефолт feature
    // тоже пишется в колонку
    let ty_canonical = match ty {
        Some("") => anyhow::bail!(
            "на create пустой тип бессмысленен — убери --type; снятие: task edit <id> --type \"\""
        ),
        Some(raw) => syt::validate_task_type(raw)?,
        None => syt::DEFAULT_TASK_TYPE.to_string(),
    };
    // приоритет: канон до любых записей (дефолта нет — решение
    // владельца «нужно явно все указывать обязательно»); пустой на
    // create бессмысленен (паритет типа)
    let priority_canonical = match priority {
        Some("") => anyhow::bail!(
            "на create пустой приоритет бессмысленен — убери --priority; снятие: task edit <id> --priority \"\""
        ),
        Some(raw) => Some(syt::validate_priority(raw)?),
        None => None,
    };
    // начальный статус до любых записей: флаг строго для машинного
    // Todo (крон-раны), всё прочее — отказ
    let initial_status = syt::validate_create_status(status)?;
    if let Some(expr) = schedule {
        if expr.is_empty() {
            anyhow::bail!(
                "на create пустое расписание бессмысленно — убери --schedule; снятие: task edit <id> --schedule \"\""
            );
        }
        crate::schedule::validate_schedule(expr)?;
    }
    if let Some(name) = agent {
        if name.is_empty() {
            anyhow::bail!(
                "на create пустой агент бессмысленен — убери --agent; снятие: task edit <id> --agent \"\""
            );
        }
        syt::validate_agent_name(name)?;
    }
    Ok((ty_canonical, priority_canonical, initial_status))
}
