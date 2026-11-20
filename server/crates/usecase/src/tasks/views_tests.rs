//! Юниты чтений для MCP (views): персона на верхнем уровне конфигурации
//! (#1214). Пины на слаг — переезд носителя закреплён: неверный слаг
//! персоны ломает person_show для всех агентских сессий.

use super::views::{PersonView, PERSON_SLUG};
use crate::error::UsecaseResult;
use crate::error::ViewError;
use crate::ports::{AppendDocOutcome, WikiRepo};
use std::sync::{Arc, Mutex};

struct MemWiki(Mutex<std::collections::HashMap<String, String>>);

#[async_trait::async_trait]
impl WikiRepo for MemWiki {
    async fn read_doc(&self, slug: &str) -> UsecaseResult<Option<String>> {
        Ok(self.0.lock().unwrap().get(slug).cloned())
    }
    async fn list_docs(&self, prefix: &str) -> UsecaseResult<Vec<String>> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .keys()
            .filter(|s| s.starts_with(prefix))
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
