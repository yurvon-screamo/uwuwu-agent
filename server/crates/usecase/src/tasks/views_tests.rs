//! Юниты чтений для MCP (views): персона на верхнем уровне конфигурации
//! (#1214). Пины на слаг — переезд носителя закреплён: неверный слаг
//! персоны ломает person_show для всех агентских сессий.

use super::views::{DocsView, PersonView, ViewError, PERSON_SLUG};
use crate::error::UsecaseResult;
use crate::ports::{AppendDocOutcome, WikiRepo};
use std::sync::{Arc, Mutex};

struct MemWiki(Mutex<std::collections::HashMap<String, String>>);

#[async_trait::async_trait]
impl WikiRepo for MemWiki {
    async fn read_doc(&self, slug: &str) -> UsecaseResult<Option<String>> {
        Ok(self.0.lock().unwrap().get(slug).cloned())
    }
    async fn list_docs(&self, prefix: &str) -> UsecaseResult<Vec<String>> {
        // Граница префикса — как адаптер (github_wiki): «{norm}/» или p == norm;
        // голый starts_with ловит соседей по стему и врёт в тестах (#1385)
        let norm = prefix.trim_matches('/');
        Ok(self
            .0
            .lock()
            .unwrap()
            .keys()
            .filter(|s| norm.is_empty() || s.starts_with(&format!("{norm}/")) || *s == norm)
            .cloned()
            .collect())
    }
    async fn doc_shas(&self) -> UsecaseResult<Vec<(String, String)>> {
        Ok(Vec::new())
    }
    async fn write_doc(&self, slug: &str, content: &str, _m: &str) -> UsecaseResult<()> {
        self.0.lock().unwrap().insert(slug.into(), content.into());
        Ok(())
    }
    async fn append_doc(
        &self,
        slug: &str,
        line: &str,
        _m: &str,
    ) -> UsecaseResult<AppendDocOutcome> {
        let mut docs = self.0.lock().unwrap();
        let entry = docs.entry(slug.into()).or_default();
        entry.push_str(&format!("{line}\n"));
        let lines = entry.lines().count();
        Ok(AppendDocOutcome {
            created: false,
            lines,
        })
    }
}

#[test]
fn person_slug_is_top_level_config() {
    // Слаг персоны — верхний уровень /person (#1214), не легаси
    // Experience/person/PERSON: константа — носитель контракта.
    assert_eq!(PERSON_SLUG, "person/README");
}

#[tokio::test]
async fn person_show_reads_top_level_readme() {
    // Главный путь: person_show читает person/README и отдаёт тело.
    let wiki = Arc::new(MemWiki(Mutex::new(std::collections::HashMap::from([(
        "person/README".into(),
        "---\ntitle: PERSON\n---\n\nЮра. Тех. Лид. Ответ на русском.\n".into(),
    )]))));
    let md = PersonView::new(wiki).show().await.unwrap();
    assert!(
        md.contains("Ответ на русском"),
        "тело персоны прочитано с нового слага: {md}"
    );
}

#[tokio::test]
async fn person_show_without_doc_is_loud_not_found() {
    // Окно переезда (до wiki-миграции): слага нет — внятный NotFound,
    // не пустая строка и не транспортный отказ.
    let wiki = Arc::new(MemWiki(Mutex::default()));
    let err = PersonView::new(wiki).show().await.unwrap_err();
    match err {
        ViewError::NotFound(what) => assert!(
            what.contains("персона"),
            "в сообщении назван отсутствующий носитель: {what}"
        ),
        other => panic!("ожидается NotFound, получено: {other:?}"),
    }
}

// --- DocsView::list: file/dir-коллизия носителей (#1385) ---

fn docs_wiki(slugs: &[&str]) -> Arc<MemWiki> {
    Arc::new(MemWiki(Mutex::new(
        slugs
            .iter()
            .map(|s| ((*s).to_string(), String::new()))
            .collect(),
    )))
}

fn sorted(mut out: Vec<String>) -> Vec<String> {
    out.sort();
    out
}

#[tokio::test]
async fn docs_ls_collision_marks_both_carriers() {
    // Пара «файл X.md + папка X/» в одном каталоге (#1385): строка X обязана
    // различимо помечать оба носителя — тихая слепота листинга запрещена.
    // Формат пометки закреплён контрактом: якорь « (неоднозначно:».
    let view = DocsView::new(docs_wiki(&["X", "X/a", "X/b"]));
    let out = sorted(view.list("X").await.unwrap());
    assert_eq!(
        out,
        vec![
            "X (неоднозначно: файл «X.md» + папка «X/»)".to_string(),
            "X/a".to_string(),
            "X/b".to_string(),
        ]
    );
}

#[tokio::test]
async fn docs_ls_collision_dash_stem_stays_parseable() {
    // Стемы с « — » в вики реальны (ADR-тайтлы): пометка не должна ломать
    // слаг-префикс — якорь реза « (неоднозначно:» уникален и не встречается
    // в слаге.
    let view = DocsView::new(docs_wiki(&["A — B", "A — B/c"]));
    let out = sorted(view.list("A — B").await.unwrap());
    assert_eq!(
        out,
        vec![
            "A — B (неоднозначно: файл «A — B.md» + папка «A — B/»)".to_string(),
            "A — B/c".to_string(),
        ]
    );
}

#[tokio::test]
async fn docs_ls_nested_collision_annotates_stem_only() {
    // Коллизия на глубине: файл a/b.md + папка a/b/ — помечается ровно
    // строка a/b, дети без изменений.
    let view = DocsView::new(docs_wiki(&["a/b", "a/b/c"]));
    let out = sorted(view.list("a").await.unwrap());
    assert_eq!(
        out,
        vec![
            "a/b (неоднозначно: файл «a/b.md» + папка «a/b/»)".to_string(),
            "a/b/c".to_string(),
        ]
    );
}

#[tokio::test]
async fn docs_ls_file_without_dir_unchanged() {
    // Файл без папки-тёзки: листинг не меняется (регрессия формата).
    let view = DocsView::new(docs_wiki(&["Y"]));
    let out = view.list("Y").await.unwrap();
    assert_eq!(out, vec!["Y".to_string()]);
}

#[tokio::test]
async fn docs_ls_dir_without_stab_has_no_stem_line() {
    // Папка без файла-стаба: дети без изменений, строки-стема нет
    // (дир-узлы строк не создают — известный квирк md-слаг-листинга,
    // вне скоупа #1385; здесь пин на отсутствие ложной пометки).
    let view = DocsView::new(docs_wiki(&["Z/a", "Z/b"]));
    let out = sorted(view.list("Z").await.unwrap());
    assert_eq!(out, vec!["Z/a".to_string(), "Z/b".to_string()]);
}

#[tokio::test]
async fn docs_ls_clean_listing_unchanged() {
    // Неколлизионный каталог: вывод байт-в-байт как раньше.
    let view = DocsView::new(docs_wiki(&["P/README", "P/Design Spec"]));
    let out = sorted(view.list("P").await.unwrap());
    assert_eq!(
        out,
        vec!["P/Design Spec".to_string(), "P/README".to_string()]
    );
}
