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
    append_with_pacing(sy, target, md, ConfirmPacing::production()).await
}

async fn append_with_pacing(
    sy: &SiYuan,
    target: &PersonTarget,
    md: &str,
    pacing: ConfirmPacing,
) -> Result<String> {
    if md.trim().is_empty() {
        bail!("append: пустой текст — нечего дописывать");
    }
    match resolve(sy, target).await? {
        Some(doc_id) => {
            append_once_confirmed(sy, &doc_id, md, pacing).await?;
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

/// Идемпотентный аппенд факта: один POST без слепых ретраев — неидемпотентная
/// запись при потерянном ответе дублируется (класс ses_e729d7329ffe, факт
/// дублировался 4 копиями). При сбое — контрольное чтение с коротким
/// поллингом (sync-лаг ядра прячет только что записанный блок): маркер в
/// доке → запись легла, успех без повтора. Пре-чека нет: легитимный
/// повторный append того же факта сохраняется («дедуп — сначала show»).
/// Все попытки мимо → честный сбой с пометкой «могло примениться» — слепой
/// повтор вызывающим опасен.
async fn append_once_confirmed(
    sy: &SiYuan,
    doc_id: &str,
    md: &str,
    pacing: ConfirmPacing,
) -> Result<()> {
    let marker = crate::siyuan::first_nonempty_line(md);
    if let Err(err) = sy.append_block_md_once(doc_id, md).await {
        for attempt in 0..pacing.attempts {
            if attempt > 0 {
                tokio::time::sleep(pacing.poll_delay).await;
            }
            let Ok(exported) = sy.export_md_by_id(doc_id).await else {
                continue;
            };
            if exported.contains(&marker) {
                return Ok(());
            }
        }
        bail!("{err}; запись могла примениться — проверь `person show` перед повтором");
    }
    Ok(())
}

/// Пейсинг контрольного чтения после сбоя append: производство с запасом на
/// sync-лаг ядра, тесты — мгновенные.
struct ConfirmPacing {
    attempts: u32,
    poll_delay: std::time::Duration,
}

impl ConfirmPacing {
    fn production() -> Self {
        Self {
            attempts: 3,
            poll_delay: std::time::Duration::from_secs(1),
        }
    }

    #[cfg(test)]
    fn instant() -> Self {
        Self {
            attempts: 3,
            poll_delay: std::time::Duration::from_millis(1),
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
    sy.overwrite_doc_md(&doc_id, target.title(), md).await?;
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
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};

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

    /// Поведение appendBlock мока: успех; «применено, ответ потерян» (запись
    /// в контенте есть, клиент видит code≠0); отказ без применения.
    #[derive(Clone, Copy)]
    enum AppendBehavior {
        Ok,
        AppliedButLost,
        Rejected,
    }

    /// Мок-док персоны: resolve (ноутбук + hpath), экспорт отдаёт текущий
    /// контент, appendBlock мутирует его и/или «роняет» ответ по behavior.
    struct PersonMock {
        server: wiremock::MockServer,
        content: Arc<Mutex<String>>,
        appends: Arc<Mutex<Vec<String>>>,
    }

    const PERSON_DOC: &str = "20261107000000-persond1";

    async fn spawn_person_mock(initial: &str, behavior: AppendBehavior) -> PersonMock {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let content = Arc::new(Mutex::new(initial.to_string()));
        let appends = Arc::new(Mutex::new(Vec::new()));

        Mock::given(method("POST"))
            .and(path("/api/notebook/lsNotebooks"))
            .respond_with(|_req: &wiremock::Request| {
                ResponseTemplate::new(200).set_body_json(json!({
                    "code": 0,
                    "data": { "notebooks": [{ "id": "nb1", "name": "Experience", "closed": false }] }
                }))
            })
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/filetree/getIDsByHPath"))
            .respond_with(|req: &wiremock::Request| {
                let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let hpath = body.get("path").and_then(|p| p.as_str()).unwrap_or("");
                let data = if hpath == "/personal/PERSON" {
                    json!([PERSON_DOC])
                } else {
                    json!([])
                };
                ResponseTemplate::new(200).set_body_json(json!({ "code": 0, "data": data }))
            })
            .mount(&server)
            .await;

        let content_for_export = content.clone();
        Mock::given(method("POST"))
            .and(path("/api/export/exportMdContent"))
            .respond_with(move |_req: &wiremock::Request| {
                let md = content_for_export.lock().expect("content lock").clone();
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "code": 0, "msg": "", "data": { "content": md } }))
            })
            .mount(&server)
            .await;

        let content_for_append = content.clone();
        let appends_log = appends.clone();
        Mock::given(method("POST"))
            .and(path("/api/block/appendBlock"))
            .respond_with(move |req: &wiremock::Request| {
                let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let data = body
                    .get("data")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string();
                if matches!(
                    behavior,
                    AppendBehavior::Ok | AppendBehavior::AppliedButLost
                ) {
                    content_for_append
                        .lock()
                        .expect("content lock")
                        .push_str(&data);
                }
                appends_log.lock().expect("appends lock").push(data);
                let code = match behavior {
                    AppendBehavior::Ok => 0,
                    AppendBehavior::AppliedButLost | AppendBehavior::Rejected => -1,
                };
                ResponseTemplate::new(200).set_body_json(json!({ "code": code, "msg": "lost" }))
            })
            .mount(&server)
            .await;

        PersonMock {
            server,
            content,
            appends,
        }
    }

    #[tokio::test]
    async fn fact_applied_but_lost_is_written_exactly_once() {
        let mock = spawn_person_mock(
            "# PERSON\n\n## Кто владелец\n\nЮра. Тех. Лид.\n",
            AppendBehavior::AppliedButLost,
        )
        .await;
        let sy = SiYuan::for_tests(mock.server.uri());

        append_with_pacing(
            &sy,
            &target(),
            "Предпочитает Rust для инструментов",
            ConfirmPacing::instant(),
        )
        .await
        .expect("запись применена, ответ потерян — контрольное чтение узнаёт факт");

        let content = mock.content.lock().expect("content lock");
        assert_eq!(
            content
                .matches("Предпочитает Rust для инструментов")
                .count(),
            1,
            "факт в доке ровно один (слепой ретрай дал бы копии): {content}"
        );
        let appends = mock.appends.lock().expect("appends lock");
        assert_eq!(
            appends.len(),
            1,
            "appendBlock вызван ровно один раз, без слепых ретраев"
        );
    }

    #[tokio::test]
    async fn fact_append_ok_writes_once_without_confirmation_roundtrip() {
        let mock = spawn_person_mock(
            "# PERSON\n\n## Кто владелец\n\nЮра. Тех. Лид.\n",
            AppendBehavior::Ok,
        )
        .await;
        let sy = SiYuan::for_tests(mock.server.uri());

        append_with_pacing(
            &sy,
            &target(),
            "Качество и процессы важнее скорости",
            ConfirmPacing::instant(),
        )
        .await
        .expect("успешная запись проходит без контрольных чтений");

        let content = mock.content.lock().expect("content lock");
        assert_eq!(
            content
                .matches("Качество и процессы важнее скорости")
                .count(),
            1,
            "happy-path без дублей: {content}"
        );
        let appends = mock.appends.lock().expect("appends lock");
        assert_eq!(appends.len(), 1, "one-shot: ровно один POST");
    }

    #[tokio::test]
    async fn fact_append_transport_failure_surfaces_without_writing() {
        let mock = spawn_person_mock(
            "# PERSON\n\n## Кто владелец\n\nЮра. Тех. Лид.\n",
            AppendBehavior::Rejected,
        )
        .await;
        let sy = SiYuan::for_tests(mock.server.uri());

        let err = append_with_pacing(
            &sy,
            &target(),
            "Уникальный факт про BDD",
            ConfirmPacing::instant(),
        )
        .await
        .expect_err("честный сбой записи не глотается");

        assert!(!format!("{err}").is_empty());
        let content = mock.content.lock().expect("content lock");
        assert!(
            !content.contains("Уникальный факт про BDD"),
            "отклонённая запись в док не попадает: {content}"
        );
        let appends = mock.appends.lock().expect("appends lock");
        assert_eq!(
            appends.len(),
            1,
            "once-запись: ровно один POST, без слепых ретраев"
        );
    }
}
