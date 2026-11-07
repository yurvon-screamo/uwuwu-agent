//! Перечисление доков по hpath-слагу (`docs ls`): штатный путь «какие доки
//! в проекте/папке» из агентской сессии — вместо слепых проб kernel ls
//! (ФС id-директорий, не человеческие имена) и ручного SQL. Адресация —
//! hpath-слаг как у read (`uwuwu`, `uwuwu/ADR`, `platform`); первый сегмент
//! hpath — логическое пространство (проект в ноутбуке Projects, топик в
//! Experience), физических ноутбуков три (Experience/Projects/Daily),
//! поэтому префикс-матч глобальный, по всем ноутбукам.

use crate::siyuan::SiYuan;
use anyhow::{anyhow, Result};
use serde_json::Value;

/// Явный LIMIT в каждом stmt обязателен: без него ядро молча отдаёт
/// первые 64 строки (orgls-урок в tools/wiki_ops.py).
const SUBTREE_LIMIT: usize = 10000;

/// Сабкоманды `docs`. Сейчас одна — ls; семья растит команду дерева доков.
pub(crate) async fn docs_run(command: super::cli::DocsSub) -> Result<()> {
    match command {
        super::cli::DocsSub::Ls { prefix } => ls_run(&prefix).await,
    }
}

pub(crate) async fn ls_run(prefix: &str) -> Result<()> {
    let sy = SiYuan::from_env()?;
    let listing = ls_listing(&sy, prefix, SUBTREE_LIMIT).await?;
    if listing.truncated {
        eprintln!(
            "docs ls: поддерево обрезано лимитом {SUBTREE_LIMIT} строк — вывод неполный"
        );
    }
    // Some(vec) пустым не приходит: ноль детей уходит в «пусто»-ветку ls_listing
    match listing.children {
        Some(entries) => println!("{}", entries.join("\n")),
        None => println!("пусто"),
    }
    Ok(())
}

/// Прямые дети hpath-префикса.
#[derive(Debug)]
pub(super) struct Listing {
    /// Имена доков и подпапок («dir/»); None — пространство известно,
    /// но дочерних доков нет («пусто»; несуществующую подпапку от пустой
    /// индекс не отличает — папки в SiYuan неявные).
    pub(super) children: Option<Vec<String>>,
    /// Поддерево упёрлось в лимит — вывод неполный (громко, не молча).
    pub(super) truncated: bool,
}

pub(super) async fn ls_listing(sy: &SiYuan, prefix_raw: &str, subtree_limit: usize) -> Result<Listing> {
    let prefix = prefix_raw.trim_matches('/');
    if prefix.is_empty() {
        let tops = top_segments(sy).await?;
        return Err(anyhow!(
            "пустой префикс — укажи <проект>[/<подпапка>]; доступные пространства: {}",
            tops.join(", ")
        ));
    }

    let rows = sy.sql(&subtree_stmt(prefix, subtree_limit + 1)).await?;
    let truncated = rows.len() > subtree_limit;
    let children = direct_children(prefix, hpaths(&rows[..subtree_limit.min(rows.len())]));
    if !children.is_empty() {
        return Ok(Listing {
            children: Some(children),
            truncated,
        });
    }

    // Ноль строк: известное пространство → «пусто»; неизвестное →
    // громкий отказ с раскладкой доступных.
    let top = prefix.split('/').next().unwrap_or(prefix);
    let tops = top_segments(sy).await?;
    if tops.iter().any(|known| known == top) {
        Ok(Listing {
            children: None,
            truncated: false,
        })
    } else {
        Err(anyhow!(
            "пространство «{top}» не найдено; доступные: {}",
            tops.join(", ")
        ))
    }
}

/// SQL выборки поддерева: substr-префикс вместо LIKE (вайлкарды %/_ в
/// именах не интерпретируются); substr в SQLite считает символы, не байты —
/// кириллические hpath требуют chars().count(). Запрашивается
/// fetch_limit = subtree_limit + 1: обрезка читается с факта перебора
/// (строк > лимита), не с угадывания «ровно лимит = обрезано».
pub(super) fn subtree_stmt(prefix: &str, fetch_limit: usize) -> String {
    let query_prefix = format!("/{}/", prefix);
    let chars = query_prefix.chars().count();
    let escaped = query_prefix.replace('\'', "''");
    format!(
        "select hpath from blocks where type='d' \
         and substr(hpath,1,{chars})='{escaped}' \
         order by hpath limit {fetch_limit}"
    )
}

/// Прямые дети из hpath-поддерева: сегмент без дальнейшего «/» — док,
/// с «/» — подпапка (имя из потомков, папки в индексе неявные); дедуп
/// потомков одной папки. Папка-док с детьми («ADR» + «ADR/») печатается
/// одной записью «ADR/» — папка и есть.
pub(super) fn direct_children<'a, I>(prefix: &str, hpaths: I) -> Vec<String>
where
    I: Iterator<Item = &'a str>,
{
    let full_prefix = format!("/{}/", prefix);
    let mut entries: Vec<String> = Vec::new();
    for hpath in hpaths {
        let Some(rest) = hpath.strip_prefix(&full_prefix) else {
            continue;
        };
        match rest.split_once('/') {
            Some((dir, _)) => entries.push(format!("{dir}/")),
            None => entries.push(rest.to_string()),
        }
    }
    entries.sort();
    entries.dedup();
    let dirs: std::collections::HashSet<String> = entries
        .iter()
        .filter(|entry| entry.ends_with('/'))
        .map(|entry| entry.trim_end_matches('/').to_string())
        .collect();
    entries.retain(|entry| entry.ends_with('/') || !dirs.contains(entry));
    entries
}

/// Первый сегмент hpath (запасной «/» в конкатенации — для одиночных
/// сегментов Daily) — только сегменты с хотя бы одним дочерним доком
/// (hpath ≥2 сегментов): раскладка доступных пространств для отказов и
/// «пусто». Одиночные hpath (доки Daily и корневые доки Experience)
/// пространствами не считаются — иначе раскладка превращается в перечень
/// сотен имён. Фильтр глубины согласован с subtree-матчем ls_listing:
/// пространство с прямыми доками без подпапок — тоже известное.
async fn top_segments(sy: &SiYuan) -> Result<Vec<String>> {
    let rows = sy.sql(top_segments_stmt()).await?;
    Ok(rows
        .iter()
        .filter_map(|row| row.get("top").and_then(|v| v.as_str()))
        .filter(|top| !top.is_empty())
        .map(String::from)
        .collect())
}

/// SQL раскладки пространств — отдельная функция ради пин-теста
/// согласованности глубин с subtree-матчем.
pub(super) fn top_segments_stmt() -> &'static str {
    "select distinct substr(hpath, 2, instr(substr(hpath, 2) || '/', '/') - 1) as top \
     from blocks where type='d' and hpath like '/%/%' order by top limit 1000"
}

fn hpaths(rows: &[Value]) -> impl Iterator<Item = &str> {
    rows.iter()
        .filter_map(|row| row.get("hpath").and_then(|v| v.as_str()))
}
