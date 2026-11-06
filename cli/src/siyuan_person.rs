//! Персона владельца: SiYuan-док (канонически Experience:/personal/PERSON),
//! читается агентами в начале работы (`person show`), пишется владельцем
//! и дримером (`person append`; replace — только осознанная полная
//! перезапись). Один инстанс SiYuan на все машины — пуш-политика и
//! конфликтов правок нет: append сериализуется ядром, дедуп — забота
//! пишущего (живое чтение перед append).
//!
//! Stateless по построению: каждая команда ходит в ядро живьём; ядра нет —
//! честная ошибка. Env-шов UWUWU_PERSON_NOTEBOOK/UWUWU_PERSON_HPATH —
//! только для e2e (канонический путь в проде не переопределяется).

use crate::markdown::strip_export_wrapper;
use crate::siyuan::SiYuan;
use anyhow::{bail, Result};

/// Ноутбук по имени — read-only (без create-or-get ensure_notebook):
/// show — команда чтения, молча создавать ноутбук ей нельзя.
async fn notebook_id_readonly(sy: &SiYuan, name: &str) -> Result<Option<String>> {
    for nb in sy.notebooks().await? {
        if !nb.get("closed").and_then(|c| c.as_bool()).unwrap_or(false)
            && nb.get("name").and_then(|n| n.as_str()) == Some(name)
        {
            return Ok(nb.get("id").and_then(|i| i.as_str()).map(String::from));
        }
    }
    Ok(None)
}

/// Канонический адрес персоны: ноутбук и hpath (env-шов для e2e).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonTarget {
    pub notebook: String,
    pub hpath: String,
}

pub fn person_target() -> PersonTarget {
    PersonTarget {
        notebook: std::env::var("UWUWU_PERSON_NOTEBOOK").unwrap_or_else(|_| "Experience".into()),
        hpath: std::env::var("UWUWU_PERSON_HPATH").unwrap_or_else(|_| "/personal/PERSON".into()),
    }
}

impl PersonTarget {
    /// Тайтл дока = последний сегмент hpath (строка `# <тайтл>` обёртки экспорта).
    pub fn title(&self) -> &str {
        self.hpath.rsplit('/').next().unwrap_or(&self.hpath)
    }
}

/// docId персоны; None — дока нет. Дубль hpath (ядерный индекс отдал
/// несколько id) — громкая ошибка: молча брать первый значит тихо потерять
/// второй; чистится руками в UI (дисциплина task create).
pub async fn resolve(sy: &SiYuan, target: &PersonTarget) -> Result<Option<String>> {
    let Some(nb) = notebook_id_readonly(sy, &target.notebook).await? else {
        return Ok(None);
    };
    let ids = sy.doc_ids_by_hpath(&nb, &target.hpath).await?;
    match ids.len() {
        0 => Ok(None),
        1 => Ok(ids.into_iter().next()),
        n => bail!(
            "док персоны {} продублирован ({} id: {}) — почисти в UI, оставь один",
            target.hpath,
            n,
            ids.join(", ")
        ),
    }
}

/// Резолв с ожиданием индекса: после createDocWithMd метаданные ядра
/// перестраиваются асинхронно — дока может быть ещё не видна запросам
/// (репро read_e2e). Поллим резолв, дубли сразу дают громкий отказ resolve.
async fn resolve_waited(sy: &SiYuan, target: &PersonTarget) -> Result<Option<String>> {
    for attempt in 0..10 {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        if let Some(doc_id) = resolve(sy, target).await? {
            return Ok(Some(doc_id));
        }
    }
    Ok(None)
}

/// Актуальная персона (markdown без экспортной обёртки).
pub async fn show(sy: &SiYuan, target: &PersonTarget) -> Result<String> {
    let doc_id = resolve(sy, target).await?.ok_or_else(person_missing)?;
    let raw = sy.export_md_by_id(&doc_id).await?;
    Ok(strip_export_wrapper(&raw, target.title()))
}

/// Дописать markdown в конец (факт/секция). Дока нет — создать со скелетом
/// и сразу с этим текстом (один атомарный createDocWithMd); создание
/// верифицируется ждущим резолвом со сверкой id (дубль hpath двух создателей
/// → громкий отказ resolve, невидимый индексом дубль — расхождением id).
/// Возвращает docId.
pub async fn append(sy: &SiYuan, target: &PersonTarget, md: &str) -> Result<String> {
    if md.trim().is_empty() {
        bail!("append: пустой текст — нечего дописывать");
    }
    match resolve(sy, target).await? {
        Some(doc_id) => {
            sy.append_block_md(&doc_id, md).await?;
            Ok(doc_id)
        }
        None => {
            let nb = sy.ensure_notebook(&target.notebook).await?;
            let combined = format!("{}\n{}", skeleton(target), md.trim_end());
            let created = sy.create_doc_md(&nb, &target.hpath, &combined).await?;
            verify_created(sy, target, &created).await
        }
    }
}

/// Верификация только что созданного дока: ждём индекс и сверяем резолвнутый
/// id с возвращённым create'ом. Совпал — наш док и дублей нет (len>1 отловлен
/// resolve); индекс видит ЧУЖОЙ id — гонка двух создателей, дубль уже есть →
/// громкий отказ; индекс никого не видит — доверяем ответу create.
async fn verify_created(sy: &SiYuan, target: &PersonTarget, created: &str) -> Result<String> {
    match resolve_waited(sy, target).await? {
        Some(doc_id) if created.is_empty() || doc_id == created => Ok(doc_id),
        Some(other) => bail!(
            "создание персоны {} задублировалось (создан {created}, индекс видит {other}) — почисти в UI, оставь один",
            target.hpath
        ),
        None if !created.is_empty() => Ok(created.to_string()),
        None => Err(person_missing()),
    }
}

/// Перезаписать контент целиком на месте (docId стабилен). Дока нет —
/// создать: контент пишет сам create, повторной перезаписи нет. Пустой
/// контент — отказ (персона не может быть пустой).
pub async fn replace(sy: &SiYuan, target: &PersonTarget, md: &str) -> Result<String> {
    if md.trim().is_empty() {
        bail!("replace: пустой контент — персона не может быть пустой; правь/удаляй док в UI");
    }
    let doc_id = match resolve(sy, target).await? {
        Some(doc_id) => doc_id,
        None => {
            let nb = sy.ensure_notebook(&target.notebook).await?;
            let created = sy.create_doc_md(&nb, &target.hpath, md).await?;
            verify_created(sy, target, &created).await?
        }
    };
    sy.overwrite_doc_md(&doc_id, md).await?;
    Ok(doc_id)
}

/// Ошибка «персоны нет»: внятный путь создания вместо пустого вывода.
fn person_missing() -> anyhow::Error {
    anyhow::anyhow!(
        "персона не заведена — создай: uwuwu-cli person append \"<факт>\" (или заполни владелец: person replace --stdin)"
    )
}

/// Скелет нового дока: границы приватности и секции. Компактный — док
/// читается агентами в каждой сессии; наполняет владелец, дример
/// поддерживает точечными append'ами.
pub fn skeleton(target: &PersonTarget) -> String {
    format!(
        "> Персона владельца платформы uwuwu ({notebook}:{hpath}). Читают агенты в начале \
работы (`uwuwu-cli person show`); пишут владелец и дример (точечные append).\n\
>\n\
> **Границы приватности** — только рабочие предпочтения и стиль. НЕ писать: \
креды/токены, адреса/телефоны, здоровье, финансы — всё, что жалко выставить \
наружу при утечке воркспейса. Долговременные факты, эфемерное — мимо; держать \
компактным (док попадает в контекст агентских сессий).\n\n\
## Кто владелец\n\n(заполняет владелец)\n\n\
## Общение: язык и стиль\n\n(язык, форма ответов, чего избегать)\n\n\
## Рабочие предпочтения\n\n(пайпы, стек, инструменты)\n\n\
## Анти-паттерны (не делать)\n\n(запреты и то, что владелец не любит)\n",
        notebook = target.notebook,
        hpath = target.hpath
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> PersonTarget {
        PersonTarget {
            notebook: "Experience".into(),
            hpath: "/personal/PERSON".into(),
        }
    }

    #[test]
    fn title_is_last_hpath_segment() {
        assert_eq!(target().title(), "PERSON");
        assert_eq!(
            PersonTarget {
                notebook: "X".into(),
                hpath: "/a/b".into()
            }
            .title(),
            "b"
        );
    }

    #[test]
    fn skeleton_has_privacy_header_and_sections() {
        let sk = skeleton(&target());
        assert!(sk.contains("Границы приватности"));
        assert!(sk.contains("НЕ писать"));
        for section in [
            "Кто владелец",
            "Общение",
            "Рабочие предпочтения",
            "Анти-паттерны",
        ] {
            assert!(sk.contains(section), "секция {section} отсутствует");
        }
        assert!(
            !sk.contains("PERSON.md"),
            "путь дока не обязан совпадать с тайтлом"
        );
    }

    #[test]
    fn skeleton_names_canonical_command() {
        assert!(skeleton(&target()).contains("uwuwu-cli person show"));
    }
}
