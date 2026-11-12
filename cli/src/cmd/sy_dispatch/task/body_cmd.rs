//! task body: запись тела (replace/append/replace-section), аттачи, снапшот
//! --base, гварды входа, контракт ответов (финальный экспорт).

use crate::cmd::cli::TaskSub;
use crate::html_embed;
use crate::markdown;
use crate::md_artifacts;
use crate::siyuan::SiYuan;
use crate::siyuan_task as syt;
use anyhow::{bail, Context, Result};

use super::helpers::{print_final_body, resolve_task};
use crate::cmd::sy_dispatch::attach::{attach_refs_md, validate_attach_files};
use crate::cmd::sy_dispatch::body::{read_base_snapshot, write_body_in_place, BodyRewrite};
use crate::cmd::sy_dispatch::guards::read_body_input;

pub(super) async fn run_body(sy: &SiYuan, project: &str, command: TaskSub) -> Result<()> {
    let TaskSub::Body {
        id,
        replace,
        replace_section,
        md,
        stdin,
        attach,
        base,
        force,
    } = command
    else {
        unreachable!("task_run роутит Body в body_cmd::run_body");
    };
    let attach_files = validate_attach_files(&attach)?;
    let content = read_body_input(md.as_deref(), stdin, !attach_files.is_empty())?;
    // гард вставки обёртки вывода task get в контент (202611080031):
    // вход пишется как есть — шапка карточки, маркер и frontmatter от
    // get-вывода ложатся в тело литерально и множатся на replace-циклах.
    // Отказ до сети и записи (no-write); при --force тоже — санитария
    // входа, инвариант, а не часть сверки свежести. Снапшот --base
    // допустим полным: cli срезает обёртку при сверке сам.
    if let Some(signature) = md_artifacts::task_get_wrapper_signature(&content) {
        bail!(
            "вход несёт обёртку вывода task get ({signature}) — она легла бы в тело \
                     литерально и множилась на replace-циклах. Скармливай контент тела: без \
                     шапки карточки «Задача:/Статус:/Строка:…», маркера «----- тело -----» и \
                     frontmatter — frontmatter дока ведёт ядро, титульный H1 пишется из \
                     заголовка дока. Снапшот для --base можно не чистить: cli срезает обёртку \
                     при сверке сам"
        );
    }
    // write-side превенция тег-матча ядра: голые решётки с пунктуацией
    // вплотную съедают бэктики код-спанов на экспорте; предупреждение
    // не блокирует запись (легитимные тела О самой порче триггерят)
    if let Some(warning) = md_artifacts::tag_trap_warning(&content) {
        eprintln!("{warning}");
    }
    // фенс-вход валидируется до resolve/upload: отказ (дубль имени,
    // незакрытый фенс, пустая строка в html) не грузит ассеты
    let fences = html_embed::parse_input(&content)?;
    // снапшот читается до обращения к ядру: локальная ошибка — fail fast
    let snapshot = read_base_snapshot(base.as_deref())?;
    let (board, task) = resolve_task(sy, project, &id).await?;
    let body_path = board.body_hpath(&task.title);
    // upload до записи тела: сбой не оставляет тело без референсов
    let mut uploaded = Vec::new();
    for file in attach_files {
        uploaded.push(sy.upload_asset(file).await?);
    }
    let doc = syt::task_body_doc(sy, &board, &task).await?;
    let doc = match doc {
        Some(doc_id) => {
            // attach-only вызов (пустой контент без rewrite) не делает
            // пустой appendBlock: опора на недокументированную реакцию
            // ядра на пустой data — референсы дописываются сами.
            // Ввод только из named-фенсов — легитимная запись без
            // residual-текста («замени диаграмму arch»).
            let rewrite = if replace || replace_section.is_some() {
                Some(BodyRewrite {
                    section: replace_section.as_deref(),
                    snapshot: snapshot.as_deref(),
                    force,
                })
            } else {
                None
            };
            if rewrite.is_some() || !content.trim().is_empty() || !fences.named_fences().is_empty()
            {
                write_body_in_place(sy, &doc_id, &task.title, rewrite, &content).await?;
            }
            doc_id
        }
        None => {
            // тела нет вовсе: создаю по заголовку (не привязываю — itemID строки неприкосновенен)
            // титульный H1 стрипается: createDocWithMd оставил бы его
            // heading-блоком — док родился бы с дублем к заголовку
            let initial = if replace || replace_section.is_some() {
                fences.inline_md()
            } else {
                fences.inline_md().trim_start().to_string()
            };
            let initial = markdown::strip_title_h1(&initial, &task.title);
            sy.create_doc_md(&board.notebook, &body_path, &initial)
                .await?
        }
    };
    if !uploaded.is_empty() {
        let refs = attach_refs_md(&uploaded);
        sy.append_block_md(&doc, &refs).await.with_context(|| {
                    format!(
                        "тело обновлено, но референсы не дописаны — повтори: task body {id} --attach <файлы>"
                    )
                })?;
    }
    println!("тело обновлено: {body_path} ({doc})");
    if !uploaded.is_empty() {
        let paths: Vec<&str> = uploaded.iter().map(|a| a.asset_path.as_str()).collect();
        println!("ассеты: {}", paths.join(", "));
    }
    // контракт ответов: финальный экспорт тела как записалось
    // (round-trip-артефакты ядра видны сразу, без контрольного get)
    print_final_body(sy, &doc, "тело").await;
    Ok(())
}
