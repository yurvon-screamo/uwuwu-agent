//! Текстовые пины поверхности MCP (#1167, допокрытие тестировщика):
//! армы вне тест-покрытия tools_tests.rs (task_create/task_edit/task_list/
//! task_get/docs_*/search/session_export + ветка unknown диспетчера).
//! Рефакторинг #1167 переносил тела байт-в-байт; пины держат наблюдаемые
//! тексты отказов и ответов, чтобы дрейф вернуть нечем. Пробы через живой
//! диспетчер на MemoryHub — без GitHub, как в tools_tests.rs.

use super::test_support::{blob_usecase, blob_usecase_missing, mcp_call, tool_hub, StubCronState};
use super::tools::ToolHub;
use serde_json::{json, Value};
use std::sync::Arc;

/// ToolHub фабрики test_support: MemoryHub как вики (docs_* армы пинов
/// читают живой носитель) + memory-blob.
fn hub() -> Arc<ToolHub> {
    let (tools, _memory) = tool_hub(
        Arc::new(crate::mockhub::MemoryHub::from_env()),
        blob_usecase(),
        Arc::new(StubCronState::default()),
    );
    tools
}

/// Живой вызов диспетчера: Ok — текст контента, Err — текст отказа.
async fn call(tools: &ToolHub, name: &str, args: Value) -> Result<String, String> {
    let msg = json!({ "params": { "name": name, "arguments": args } });
    match tools.call(&msg).await {
        Ok(v) => Ok(v[0]["text"].as_str().unwrap_or_default().to_string()),
        Err(e) => Err(e),
    }
}

/// Баг-пин дрейфа ревью-раунда 1 (H-1): множественное «обязательны»
/// из require_args не должно подменять мастерский текст tuла search.
#[tokio::test]
async fn search_without_query_keeps_master_error_text() {
    let tools = hub();
    let err = call(&tools, "search", json!({})).await.expect_err("отказ");
    assert_eq!(err, "query is required");
}

#[tokio::test]
async fn docs_append_requires_slug_and_line() {
    let tools = hub();
    let err = call(&tools, "docs_append", json!({}))
        .await
        .expect_err("отказ");
    assert_eq!(err, "slug and line are required");
}

/// Гард append-семантики docs_append (#1353): дописывание сохраняет
/// тело существующего дока (fm + контент), строка — в конец. Сид несёт
/// fm явно (#1444: вход с fm пишется дословно — append-пин фиксирует
/// append-семантику на fm-доке).
#[tokio::test]
async fn docs_append_keeps_body_of_existing_doc() {
    let tools = hub();
    let slug = "Experience/devops/Опыт (srv, #1248)";
    let seeded = "---\ntitle: Опыт (srv, #1248)\ndate: 2026-11-19\n---\n\nстрока A\n";
    call(
        &tools,
        "docs_write",
        json!({ "slug": slug, "content": seeded }),
    )
    .await
    .expect("write");
    let answer = call(
        &tools,
        "docs_append",
        json!({ "slug": slug, "line": "строка B" }),
    )
    .await
    .expect("append");
    assert!(
        answer.contains("appended to") && answer.contains("lines in the doc: 7"),
        "ответ append без факта операции: {answer}"
    );
    let stored = call(&tools, "docs_read", json!({ "slug": slug }))
        .await
        .expect("read");
    // append_to нормализует EOF: итог всегда с завершающим переносом
    assert_eq!(stored, format!("{seeded}строка B\n"));
}

/// Слаг-гард (#1613, развилка Б): append на несуществующем доке —
/// громкий отказ append-nonexistent со строкой-лекарством; файл не
/// создаётся (класс «ловится только docs_ls после записи» умер).
#[tokio::test]
async fn docs_append_refuses_missing_doc_with_creation_hint() {
    let tools = hub();
    let slug = "Experience/platform/нет-дока";
    let err = call(
        &tools,
        "docs_append",
        json!({ "slug": slug, "line": "строка" }),
    )
    .await
    .expect_err("отказ");
    assert!(
        err.contains("append-nonexistent"),
        "ветка не названа в отказе: {err}"
    );
    assert!(
        err.contains("create it via docs_write"),
        "лекарство не названо в отказе: {err}"
    );
    let read = call(&tools, "docs_read", json!({ "slug": slug })).await;
    assert!(read.is_err(), "append создал файл: {read:?}");
}

/// Append по пути-суффиксу живой статьи: отказ называет кандидата
/// (suffix_candidates), файл-сирота не создаётся.
#[tokio::test]
async fn docs_append_refuses_suffix_of_live_article_with_candidate() {
    let tools = hub();
    let live_slug = "Experience/cli/статья";
    call(
        &tools,
        "docs_write",
        json!({ "slug": live_slug, "content": "# статья\n" }),
    )
    .await
    .expect("seed");
    let err = call(
        &tools,
        "docs_append",
        json!({ "slug": "cli/статья", "line": "строка" }),
    )
    .await
    .expect_err("отказ");
    assert!(
        err.contains("append-nonexistent"),
        "ветка не названа в отказе: {err}"
    );
    assert!(
        err.contains(live_slug),
        "кандидат не назван в отказе: {err}"
    );
    let read = call(&tools, "docs_read", json!({ "slug": "cli/статья" })).await;
    assert!(read.is_err(), "append создал файл-сироту: {read:?}");
}

// --- слаг-гард docs_write (#1613): ветки отказа на достоверно новом доке ---

#[tokio::test]
async fn docs_write_refuses_unknown_root_without_file() {
    // подкласс «уникальная статья в чужом корне» #1219/#1575: хвост не совпал
    let tools = hub();
    let err = call(
        &tools,
        "docs_write",
        json!({ "slug": "cli/уникальная", "content": "# x\n" }),
    )
    .await
    .expect_err("отказ");
    assert!(
        err.contains("root-prefix"),
        "ветка не названа в отказе: {err}"
    );
    assert!(
        err.contains("Experience, Projects, bot, cron, person"),
        "канонический набор корня не назван: {err}"
    );
    let read = call(&tools, "docs_read", json!({ "slug": "cli/уникальная" })).await;
    assert!(read.is_err(), "файл вне структуры создан: {read:?}");
}

#[tokio::test]
async fn docs_write_root_refusal_carries_suffix_candidates() {
    // класс «совпавший тайтл» #1219: кандидат называет живую статью
    let tools = hub();
    let live_slug = "Experience/platform/ops-mechanics";
    call(
        &tools,
        "docs_write",
        json!({ "slug": live_slug, "content": "# om\n" }),
    )
    .await
    .expect("seed");
    let err = call(
        &tools,
        "docs_write",
        json!({ "slug": "platform/ops-mechanics", "content": "# om\n" }),
    )
    .await
    .expect_err("отказ");
    assert!(
        err.contains("root-prefix"),
        "ветка не названа в отказе: {err}"
    );
    assert!(
        err.contains(&format!("suffix candidates: {live_slug}")),
        "кандидаты не названы в отказе: {err}"
    );
    let read = call(
        &tools,
        "docs_read",
        json!({ "slug": "platform/ops-mechanics" }),
    )
    .await;
    assert!(read.is_err(), "файл-двойник создан: {read:?}");
}

#[tokio::test]
async fn docs_write_refuses_single_segment_target() {
    // L-5 р3: файл в корне вики — та же семья «файл рядом с папкой»
    let tools = hub();
    let err = call(
        &tools,
        "docs_write",
        json!({ "slug": "Experience", "content": "# x\n" }),
    )
    .await
    .expect_err("отказ");
    assert!(
        err.contains("root-prefix"),
        "ветка не названа в отказе: {err}"
    );
    let read = call(&tools, "docs_read", json!({ "slug": "Experience" })).await;
    assert!(read.is_err(), "файл в корне вики создан: {read:?}");
}

#[tokio::test]
async fn docs_write_refuses_plain_file_parent() {
    let tools = hub();
    call(
        &tools,
        "docs_write",
        json!({ "slug": "Projects/uwuwu/Design Spec", "content": "# ds\n" }),
    )
    .await
    .expect("seed");
    let err = call(
        &tools,
        "docs_write",
        json!({ "slug": "Projects/uwuwu/Design Spec/док", "content": "# d\n" }),
    )
    .await
    .expect_err("отказ");
    assert!(
        err.contains("parent-collision"),
        "ветка не названа в отказе: {err}"
    );
    assert!(
        err.contains("Projects/uwuwu/Design Spec"),
        "родитель не назван в отказе: {err}"
    );
    let read = call(
        &tools,
        "docs_read",
        json!({ "slug": "Projects/uwuwu/Design Spec/док" }),
    )
    .await;
    assert!(read.is_err(), "файл под файлом создан: {read:?}");
}

#[tokio::test]
async fn docs_write_allows_legacy_stem_parent() {
    // H-1 р1: 20 живых пар «ADR.md + ADR/» — файл и папка одновременно;
    // новый док под легаси-стемом пишется, отказ запрещён. Сеяние пары —
    // ребёнок до файла: иначе ребёнок честно ловит parent-collision
    // (родитель ещё файл без детей).
    let tools = hub();
    call(
        &tools,
        "docs_write",
        json!({ "slug": "Projects/uwuwu/ADR/ADR-001", "content": "# A1\n" }),
    )
    .await
    .expect("seed ребёнка");
    call(
        &tools,
        "docs_write",
        json!({ "slug": "Projects/uwuwu/ADR", "content": "# ADR\n" }),
    )
    .await
    .expect("seed файла");
    let answer = call(
        &tools,
        "docs_write",
        json!({ "slug": "Projects/uwuwu/ADR/ADR-114: Новый", "content": "# A114\n" }),
    )
    .await
    .expect("легаси-стем не должен отказывать");
    assert!(answer.contains("written"), "запись не состоялась: {answer}");
}

#[tokio::test]
async fn docs_write_refuses_suffix_collision_under_canonical_root() {
    // вырожденный класс вложенного корнеимени (SuffixCollision-ветка)
    let tools = hub();
    call(
        &tools,
        "docs_write",
        json!({ "slug": "Experience/Projects/x", "content": "# x\n" }),
    )
    .await
    .expect("seed");
    let err = call(
        &tools,
        "docs_write",
        json!({ "slug": "Projects/x", "content": "# x\n" }),
    )
    .await
    .expect_err("отказ");
    assert!(
        err.contains("suffix-collision"),
        "ветка не названа в отказе: {err}"
    );
    assert!(
        err.contains("Experience/Projects/x"),
        "кандидат не назван в отказе: {err}"
    );
    let read = call(&tools, "docs_read", json!({ "slug": "Projects/x" })).await;
    assert!(read.is_err(), "файл-двойник создан: {read:?}");
}

#[tokio::test]
async fn docs_write_sanitizes_slug_like_docs_read() {
    // C-2 р1: асимметрия read/write закрыта — таргет нормализуется тем
    // же sanitize_slug; двойник x.md.md не рождается
    let tools = hub();
    let answer = call(
        &tools,
        "docs_write",
        json!({ "slug": "docs/Experience/platform/x.md", "content": "# x\n" }),
    )
    .await
    .expect("нормализованный слаг не должен отказывать");
    assert!(
        answer.contains("«Experience/platform/x»"),
        "ответ не называет нормализованный слаг: {answer}"
    );
    let ls = call(&tools, "docs_ls", json!({ "path": "Experience/platform" }))
        .await
        .expect("ls");
    // адрес записи: слаг «x» жив под каноничным префиксом; сырой слаг
    // («docs/Experience/platform/x.md» → docs/docs/…md.md) под ним не виден
    assert!(
        ls.lines().any(|l| l == "Experience/platform/x"),
        "док по нормализованному слагу отсутствует: {ls}"
    );
    assert!(
        !ls.lines().any(|l| l.ends_with(".md")),
        "двойник .md.md создан: {ls}"
    );
}

#[tokio::test]
async fn docs_write_degraded_tree_keeps_root_check_and_flags_skip() {
    // деградационная строка таблицы сбоев: root жив без дерева,
    // parent/suffix пропущены, клоза называет пропуск честно
    let (_serial, _guard, records) = scoped_trap();
    let (tools, _) = tool_hub(
        Arc::new(TreeFailingWiki),
        blob_usecase(),
        Arc::new(StubCronState::default()),
    );
    let err = call(
        &tools,
        "docs_write",
        json!({ "slug": "cli/x", "content": "# x\n" }),
    )
    .await
    .expect_err("отказ");
    assert!(
        err.contains("root-prefix"),
        "root-проверка не пережила деградацию дерева: {err}"
    );
    let answer = call(
        &tools,
        "docs_write",
        json!({ "slug": "Experience/platform/y", "content": "# y\n" }),
    )
    .await
    .expect("запись в шторм не должна блокироваться");
    assert!(
        answer.contains("collision checks skipped (tree unavailable)"),
        "клоза деградации не названа в ответе: {answer}"
    );
    assert_event(&records.lock().expect("trap lock"), "slug_guard.degraded");
}

/// Вики с мёртвым деревом: list_docs рвётся транспортом (502-шторм,
/// инцидент 22–23.11) — read/write живы; append-таргет мока отсутствует
/// (писатель авторитетен), деградация только коллизий.
struct TreeFailingWiki;
#[async_trait::async_trait]
impl uwuwu_usecase::WikiRepo for TreeFailingWiki {
    async fn doc_shas(&self) -> uwuwu_usecase::UsecaseResult<Vec<(String, String)>> {
        Ok(vec![])
    }
    async fn read_doc(&self, _slug: &str) -> uwuwu_usecase::UsecaseResult<Option<String>> {
        Ok(None)
    }
    async fn list_docs(&self, _prefix: &str) -> uwuwu_usecase::UsecaseResult<Vec<String>> {
        Err(uwuwu_usecase::UsecaseError::Board(
            "wiki transport down: tree".into(),
        ))
    }
    async fn read_doc_with_sha(
        &self,
        slug: &str,
    ) -> uwuwu_usecase::UsecaseResult<Option<uwuwu_usecase::DocSnapshot>> {
        Ok(Some(uwuwu_usecase::DocSnapshot {
            content: "# dead tree\n".into(),
            blob_sha: format!("stub-{slug}"),
        }))
    }
    async fn delete_doc(
        &self,
        _slug: &str,
        _old_blob_sha: &str,
        _message: &str,
    ) -> uwuwu_usecase::UsecaseResult<uwuwu_usecase::DocDeleteCommit> {
        Err(uwuwu_usecase::UsecaseError::PrecheckUnavailable(
            "tree: HTTP 502".into(),
        ))
    }
    async fn write_doc(&self, _: &str, _: &str, _: &str) -> uwuwu_usecase::UsecaseResult<()> {
        Ok(())
    }
    async fn append_doc(&self, _: &str, _: &str, _: &str) -> uwuwu_usecase::UsecaseResult<usize> {
        Err(uwuwu_usecase::UsecaseError::AppendMissing(
            "doc does not exist (tree-failing wiki)".into(),
        ))
    }
}

#[tokio::test]
async fn docs_write_existing_doc_skips_tree() {
    // горячий путь write (существующий док): быстрый путь без пробы дерева
    let wiki = Arc::new(CountingWiki::new());
    let (tools, _) = tool_hub(
        wiki.clone(),
        blob_usecase(),
        Arc::new(StubCronState::default()),
    );
    call(
        &tools,
        "docs_write",
        json!({ "slug": "Experience/x", "content": "# x\n" }),
    )
    .await
    .expect("seed");
    wiki.reset_lists();
    let answer = call(
        &tools,
        "docs_write",
        json!({ "slug": "Experience/x", "content": "# x2\n" }),
    )
    .await
    .expect("write");
    assert!(answer.contains("written"), "запись не состоялась: {answer}");
    assert_eq!(
        wiki.list_calls(),
        0,
        "быстрый путь существующего дока запросил дерево"
    );
}

#[tokio::test]
async fn docs_append_existing_doc_skips_tree() {
    // горячий путь append (живой док): дерево в append не участвует
    let wiki = Arc::new(CountingWiki::new());
    let (tools, _) = tool_hub(
        wiki.clone(),
        blob_usecase(),
        Arc::new(StubCronState::default()),
    );
    call(
        &tools,
        "docs_write",
        json!({ "slug": "Experience/y", "content": "# y\n" }),
    )
    .await
    .expect("seed");
    wiki.reset_lists();
    let answer = call(
        &tools,
        "docs_append",
        json!({ "slug": "Experience/y", "line": "строка" }),
    )
    .await
    .expect("append");
    assert!(
        answer.contains("appended to"),
        "append не состоялся: {answer}"
    );
    assert_eq!(wiki.list_calls(), 0, "горячий путь append запросил дерево");
}

/// Счётчик обращений к дереву вики: пины «дерево не запрашивается» на
/// горячих путях (быстрый путь write существующего, append к живому).
struct CountingWiki {
    inner: crate::mockhub::MemoryHub,
    lists: std::sync::atomic::AtomicUsize,
}

impl CountingWiki {
    fn new() -> Self {
        Self {
            inner: crate::mockhub::MemoryHub::from_env(),
            lists: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn list_calls(&self) -> usize {
        self.lists.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn reset_lists(&self) {
        self.lists.store(0, std::sync::atomic::Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl uwuwu_usecase::WikiRepo for CountingWiki {
    async fn doc_shas(&self) -> uwuwu_usecase::UsecaseResult<Vec<(String, String)>> {
        self.inner.doc_shas().await
    }
    async fn read_doc(&self, slug: &str) -> uwuwu_usecase::UsecaseResult<Option<String>> {
        self.inner.read_doc(slug).await
    }
    async fn list_docs(&self, prefix: &str) -> uwuwu_usecase::UsecaseResult<Vec<String>> {
        self.lists.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.list_docs(prefix).await
    }
    async fn write_doc(
        &self,
        slug: &str,
        content: &str,
        message: &str,
    ) -> uwuwu_usecase::UsecaseResult<()> {
        self.inner.write_doc(slug, content, message).await
    }
    async fn append_doc(
        &self,
        slug: &str,
        line: &str,
        message: &str,
    ) -> uwuwu_usecase::UsecaseResult<usize> {
        self.inner.append_doc(slug, line, message).await
    }
}

/// #1576: снесённый журнал крона не воскресает через docs_append —
/// create-on-missing (#1353) превращал удаление носителя в тихую запись.
#[tokio::test]
async fn docs_append_refuses_removed_cron_journal() {
    let tools = hub();
    let slug = "cron/Loop-надёжность-конвейера/Журнал";
    let err = call(
        &tools,
        "docs_append",
        json!({ "slug": slug, "line": "ПРОБА #1576" }),
    )
    .await
    .expect_err("запись в снесённый журнал обязана отказать");
    assert!(
        err.contains("writing refused") && err.contains("#1576"),
        "отказ без причины: {err}"
    );
    let err_read = call(&tools, "docs_read", json!({ "slug": slug }))
        .await
        .expect_err("журнал не должен создаться заново");
    assert!(
        err_read.contains("not found"),
        "неожиданный док: {err_read}"
    );
}

/// #1576: тот же запрет для docs_write — slug_guard #1613 разрешает корень
/// `cron`, поэтому снесённый журнал мог создаваться полной перезаписью.
#[tokio::test]
async fn docs_write_refuses_removed_cron_journal() {
    let tools = hub();
    let slug = "cron/Sentry-ночной-просмотр/Журнал";
    let err = call(
        &tools,
        "docs_write",
        json!({
            "slug": slug,
            "content": "---\ntitle: X\ndate: 2026-11-25\n---\nПРОБА #1576",
        }),
    )
    .await
    .expect_err("запись в снесённый журнал обязана отказать");
    assert!(
        err.contains("writing refused") && err.contains("#1576"),
        "отказ без причины: {err}"
    );
    // код-ревью Low-1 (#1576): «отказ до мутации» для docs_write
    // закреплён так же, как для docs_append — гард не должен переехать
    // после write_doc и оставить созданный файл при громком отказе.
    let err_read = call(&tools, "docs_read", json!({ "slug": slug }))
        .await
        .expect_err("журнал не должен создаться заново через docs_write");
    assert!(
        err_read.contains("not found"),
        "неожиданный док: {err_read}"
    );
}

/// #1576 приёмка на проводе MCP: то, что видит живой агент-писатель —
/// оба мутационных тула вики отвечают isError с текстом гарда, и носитель
/// цел (тем же проводом journal-слаг не читается). Маршрутная интеграция
/// in-memory (роутер + MemoryHub, без сокета), по образцу пина `mcp.rs`
/// `contentless_mutations_refuse_loudly_over_the_wire_and_carrier_survives`.
#[tokio::test]
async fn removed_cron_journal_refuses_loudly_over_mcp_wire() {
    let tools = hub();
    let state = Arc::new(super::McpState {
        bearer: Arc::new("t-bearer".into()),
        tools,
    });
    let app = super::mcp_router(state);

    for (name, args, slug) in [
        (
            "docs_append",
            json!({"slug": "cron/Loop-надёжность-конвейера/Журнал", "line": "провода"}),
            "cron/Loop-надёжность-конвейера/Журнал",
        ),
        (
            "docs_write",
            json!({
                "slug": "cron/Sentry-ночной-просмотр/Журнал",
                "content": "---\ntitle: X\ndate: 2026-11-25\n---\nпровода",
            }),
            "cron/Sentry-ночной-просмотр/Журнал",
        ),
    ] {
        let answer = mcp_call(&app, "t-bearer", name, args).await;
        assert_eq!(
            answer["result"]["isError"],
            Value::Bool(true),
            "{name}: {answer}"
        );
        let text = answer["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default();
        assert!(
            text.contains("writing refused") && text.contains("#1576"),
            "{name}: {text}"
        );
        let read = mcp_call(&app, "t-bearer", "docs_read", json!({ "slug": slug })).await;
        assert_eq!(
            read["result"]["isError"],
            Value::Bool(true),
            "журнал создан: {read}"
        );
        let read_text = read["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default();
        assert!(read_text.contains("not found"), "{read_text}");
    }
}

/// #1576 / Common-1 тест-ревью: инвариант развязки конфликта с мастером
/// (#509 normalizes slug на write-путях). Гард обязан смотреть на слаг
/// ПОСЛЕ нормализации: если при развязке `refuse` останется на сыром
/// аргументе, формы `docs/cron/<имя>/Журнал`, с пробелами по краям, с
/// ведущим `/` и с пустым сегментом станут каноническим журналом — пин
/// краснеет чтением канонического слага. На ветке без #509 эти формы
/// пишут по другим путям — пин зелёный и не ловит ложных срабатываний.
#[tokio::test]
async fn normalized_journal_slugs_do_not_recreate_removed_journal() {
    let tools = hub();
    let fm = "---\ntitle: X\ndate: 2026-11-25\n---\nPIN #1576";

    // Чистая форма и .md-вариант отказывают текстом гарда.
    for raw in ["cron/Нормализация/Журнал", "cron/Нормализация/Журнал.md"]
    {
        let err = call(&tools, "docs_write", json!({ "slug": raw, "content": fm }))
            .await
            .expect_err("журнал обязан отказать в любой форме");
        assert!(
            err.contains("writing refused") && err.contains("#1576"),
            "{raw}: {err}"
        );
    }
    let read = call(
        &tools,
        "docs_read",
        json!({ "slug": "cron/Нормализация/Журнал" }),
    )
    .await
    .expect_err("чистая форма создала журнал");
    assert!(read.contains("not found"), "{read}");

    // Нормализуемые формы: сам write может отказать или уйти по другому
    // пути — важен инвариант: канонический журнал не читается.
    for (i, shape) in [
        "docs/cron/{name}/Журнал",
        " cron/{name}/Журнал ",
        "/cron/{name}/Журнал",
        "cron//{name}/Журнал",
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("Нормализация2-{i}");
        let raw = shape.replace("{name}", &name);
        let canonical = format!("cron/{name}/Журнал");
        let _ = call(&tools, "docs_write", json!({ "slug": raw, "content": fm })).await;
        match call(&tools, "docs_read", json!({ "slug": canonical })).await {
            Ok(doc) => panic!("{raw}: канонический журнал создан: {doc}"),
            Err(e) => assert!(e.contains("not found"), "{raw}: {e}"),
        }
    }
}

#[tokio::test]
async fn docs_write_requires_slug_and_content() {
    let tools = hub();
    let err = call(&tools, "docs_write", json!({ "slug": "x/y" }))
        .await
        .expect_err("отказ");
    assert_eq!(err, "slug and content are required");
}

// --- слаг-гард: вырожденные входы и окна источников (Low р1-кода) ---

#[tokio::test]
async fn docs_write_refuses_slug_empty_after_sanitization() {
    // план-ревью р3 L-5: слаг, пустой после санитайза, — отказ входа
    let tools = hub();
    let err = call(
        &tools,
        "docs_write",
        json!({ "slug": "   ", "content": "# x\n" }),
    )
    .await
    .expect_err("отказ");
    assert!(
        err.contains("nothing left after sanitization"),
        "вырожденный слаг не назван: {err}"
    );
}

#[tokio::test]
async fn docs_write_docs_prefixed_foreign_root_refused() {
    // гиркин-сценарий: docs/-форма с неканоничным корнем (санитайз стрипит
    // только ведущий docs/) — отказ root-prefix, не запись
    let tools = hub();
    let err = call(
        &tools,
        "docs_write",
        json!({ "slug": "docs/platform/x", "content": "# x\n" }),
    )
    .await
    .expect_err("отказ");
    assert!(
        err.contains("root-prefix"),
        "ветка не названа в отказе: {err}"
    );
    let read = call(&tools, "docs_read", json!({ "slug": "platform/x" })).await;
    assert!(read.is_err(), "файл вне структуры создан: {read:?}");
}

#[tokio::test]
async fn docs_append_sanitizes_slug_like_docs_read() {
    // нормализация таргета на append: docs/+.md-форма попадает в живой док
    let tools = hub();
    let live_slug = "Experience/platform/x";
    call(
        &tools,
        "docs_write",
        json!({ "slug": live_slug, "content": "# x\n" }),
    )
    .await
    .expect("seed");
    let answer = call(
        &tools,
        "docs_append",
        json!({ "slug": "docs/Experience/platform/x.md", "line": "строка" }),
    )
    .await
    .expect("нормализованный append");
    assert!(
        answer.contains("«Experience/platform/x»"),
        "ответ не называет нормализованный слаг: {answer}"
    );
}

#[tokio::test]
async fn docs_append_degraded_tree_refuses_without_candidates() {
    // строка 4 таблицы сбоев: дерево Err + append на несуществующем —
    // отказ append-nonexistent без кандидатов (отказ-факт от дерева
    // не зависит, писатель авторитетен)
    let (_serial, _guard, records) = scoped_trap();
    let (tools, _) = tool_hub(
        Arc::new(TreeFailingWiki),
        blob_usecase(),
        Arc::new(StubCronState::default()),
    );
    let err = call(
        &tools,
        "docs_append",
        json!({ "slug": "cli/x", "line": "строка" }),
    )
    .await
    .expect_err("отказ");
    assert!(
        err.contains("append-nonexistent"),
        "ветка не названа в отказе: {err}"
    );
    assert!(
        !err.contains("suffix candidates"),
        "кандидаты из мёртвого дерева: {err}"
    );
    assert_event(&records.lock().expect("trap lock"), "slug_guard.degraded");
}

// --- fm-гвард docs_write (#1444): шапка вики-дока переживает replace ---

/// Ветка Carried: шапка существующего дока переносится при правке без fm
/// во входе; date и прочие ключи целы, lastmod освежается, тело — из входа.
#[tokio::test]
async fn docs_write_carries_fm_header_of_existing_doc() {
    let tools = hub();
    let slug = "Experience/platform/ops-mechanics";
    let seeded = "---\ntitle: ops-mechanics\ndate: 2026-11-04T22:41:07+09:00\nlastmod: 2026-11-20T20:50:00+09:00\n---\n\n# ops-mechanics\n\nстарое тело\n";
    call(
        &tools,
        "docs_write",
        json!({ "slug": slug, "content": seeded }),
    )
    .await
    .expect("seed: вход с fm пишется дословно");
    let answer = call(
        &tools,
        "docs_write",
        json!({ "slug": slug, "content": "# ops-mechanics\n\nновое тело\n" }),
    )
    .await
    .expect("write");
    assert!(
        answer.contains("carried over"),
        "ветка переноса не названа в ответе: {answer}"
    );
    let stored = call(&tools, "docs_read", json!({ "slug": slug }))
        .await
        .expect("read");
    assert!(
        stored.starts_with("---\ntitle: ops-mechanics\n"),
        "шапка снесена: {stored}"
    );
    assert!(
        stored.contains("date: 2026-11-04T22:41:07+09:00"),
        "date потерян: {stored}"
    );
    assert!(
        !stored.contains("lastmod: 2026-11-20T20:50"),
        "lastmod не освежён моментом записи: {stored}"
    );
    assert!(
        stored.contains("lastmod: 2"),
        "lastmod отсутствует: {stored}"
    );
    assert!(
        stored.contains("новое тело") && !stored.contains("старое тело"),
        "тело не заменено: {stored}"
    );
}

/// Ветка Carried без lastmod в блоке (системный случай C-2: крон-мастера
/// и бот-доки): ключ дописывается последней строкой блока, операционные
/// ключи переносится байт-в-байт.
#[tokio::test]
async fn docs_write_appends_lastmod_to_fm_block_without_it() {
    let tools = hub();
    let slug = "cron/QA-ночной-аудит-кода/README";
    let seeded = "---\ntitle: QA: ночной аудит\nschedule: 30 1 * * *\nagent: tester\nmigrated-from: '#114'\nproject: uwuwu\n---\n\n## Инструкция\n\nтело\n";
    call(
        &tools,
        "docs_write",
        json!({ "slug": slug, "content": seeded }),
    )
    .await
    .expect("seed");
    let answer = call(
        &tools,
        "docs_write",
        json!({ "slug": slug, "content": "## Инструкция\n\nновое тело\n" }),
    )
    .await
    .expect("write");
    assert!(
        answer.contains("carried over"),
        "ветка переноса не названа в ответе: {answer}"
    );
    let stored = call(&tools, "docs_read", json!({ "slug": slug }))
        .await
        .expect("read");
    for key in [
        "title: QA: ночной аудит",
        "schedule: 30 1 * * *",
        "agent: tester",
        "migrated-from: '#114'",
        "project: uwuwu",
    ] {
        assert!(stored.contains(key), "ключ «{key}» потерян: {stored}");
    }
    assert!(
        stored.contains("project: uwuwu\nlastmod: 2"),
        "lastmod дописан не последней строкой блока: {stored}"
    );
}

/// Ветка Created: новый док без шапки получает каноническую; title с «: »
/// в значении сериализуется в двойных кавычках (C-3, канон ADR #1225).
#[tokio::test]
async fn docs_write_creates_canonical_header_with_quoted_title_on_colon() {
    let tools = hub();
    let slug = "Projects/uwuwu/ADR/ADR-test-guard: Заголовок решения";
    let answer = call(
        &tools,
        "docs_write",
        json!({ "slug": slug, "content": "# ADR-test-guard\n\nтекст\n" }),
    )
    .await
    .expect("write");
    assert!(
        answer.contains("canonical"),
        "ветка создания не названа в ответе: {answer}"
    );
    let stored = call(&tools, "docs_read", json!({ "slug": slug }))
        .await
        .expect("read");
    assert!(
        stored.starts_with("---\ntitle: \"ADR-test-guard: Заголовок решения\"\n"),
        "каноническая шапка без кавычек в title с «: »: {stored}"
    );
    assert!(
        stored.contains("\ndate: 2") && stored.contains("\nlastmod: 2"),
        "date/lastmod отсутствуют: {stored}"
    );
}

/// Ветка Verbatim: вход с собственным fm пишется дословно, байт-в-байт.
#[tokio::test]
async fn docs_write_keeps_fm_from_payload_verbatim() {
    let tools = hub();
    let slug = "bot/Помощник/README";
    let payload = "---\ntitle: uwuwu-помощник\nenabled: true\nmodel: opencode-go/hy4-flash-free\ntoken_env: UWUWU_BOT_TOKEN_HELPER\nowner: yurvon_screamo\n---\n\nметодичка\n";
    let answer = call(
        &tools,
        "docs_write",
        json!({ "slug": slug, "content": payload }),
    )
    .await
    .expect("write");
    assert!(
        answer.contains("verbatim"),
        "ветка дословной записи не названа в ответе: {answer}"
    );
    let stored = call(&tools, "docs_read", json!({ "slug": slug }))
        .await
        .expect("read");
    assert_eq!(stored, payload, "вход с fm искажён");
}

/// Сбой чтения существующего дока: вход без fm — громкий отказ (гварду
/// нечего переносить, молчаливая запись канонической шапки поверх
/// операционной недопустима); вход с fm — дословно, от чтения не зависит.
#[tokio::test]
async fn docs_write_read_failure_is_loud_without_fm_payload() {
    let (tools, _) = tool_hub(
        Arc::new(ReadFailingWiki),
        blob_usecase(),
        Arc::new(StubCronState::default()),
    );
    let err = call(
        &tools,
        "docs_write",
        json!({ "slug": "Experience/y", "content": "тело без шапки\n" }),
    )
    .await
    .expect_err("отказ");
    assert!(
        err.contains("fm-guard"),
        "отказ без вменяемого текста гварда: {err}"
    );
    let payload = "---\ntitle: y\n---\n\nтело с шапкой\n";
    let answer = call(
        &tools,
        "docs_write",
        json!({ "slug": "Experience/y", "content": payload }),
    )
    .await
    .expect("вход с fm не зависит от сбоя чтения");
    assert!(answer.contains("verbatim"), "ветка не названа: {answer}");
}

/// Окно сбоя чтения (#1613, ревью Low-1): существование неизвестно,
/// полный гард невозможен — но root-проверка (ни от чтения, ни от
/// дерева) остаётся: тихого пути семьи не остаётся.
#[tokio::test]
async fn docs_write_read_failure_keeps_root_check_with_fm() {
    let (tools, _) = tool_hub(
        Arc::new(ReadFailingWiki),
        blob_usecase(),
        Arc::new(StubCronState::default()),
    );
    let err = call(
        &tools,
        "docs_write",
        json!({ "slug": "platform/x", "content": "---\ntitle: x\n---\n\nтело\n" }),
    )
    .await
    .expect_err("отказ");
    assert!(
        err.contains("root-prefix"),
        "root-проверка не пережила сбой чтения: {err}"
    );
}

/// Вики-носитель с мёртвым чтением: write проходит, read рвётся транспортом.
struct ReadFailingWiki;
#[async_trait::async_trait]
impl uwuwu_usecase::WikiRepo for ReadFailingWiki {
    async fn doc_shas(&self) -> uwuwu_usecase::UsecaseResult<Vec<(String, String)>> {
        Ok(vec![])
    }
    async fn read_doc(&self, slug: &str) -> uwuwu_usecase::UsecaseResult<Option<String>> {
        Err(uwuwu_usecase::UsecaseError::Board(format!(
            "wiki transport down: {slug}"
        )))
    }
    async fn list_docs(&self, _prefix: &str) -> uwuwu_usecase::UsecaseResult<Vec<String>> {
        Ok(vec![])
    }
    async fn write_doc(
        &self,
        _slug: &str,
        _content: &str,
        _message: &str,
    ) -> uwuwu_usecase::UsecaseResult<()> {
        Ok(())
    }
    async fn append_doc(&self, _: &str, _: &str, _: &str) -> uwuwu_usecase::UsecaseResult<usize> {
        Ok(0)
    }
}

/// Гвард пустого патча через диспетчер: TaskPatch::is_empty в домене.
#[tokio::test]
async fn task_edit_empty_patch_is_loudly_rejected() {
    let tools = hub();
    let err = call(&tools, "task_edit", json!({ "number": 1 }))
        .await
        .expect_err("отказ");
    assert_eq!(err, "empty patch: set at least one field");
}

#[tokio::test]
async fn task_get_requires_number() {
    let tools = hub();
    let err = call(&tools, "task_get", json!({}))
        .await
        .expect_err("отказ");
    assert_eq!(err, "number is required (an integer)");
}

/// need_number принимает и число, и строку-число — наблюдаемое удобство.
#[tokio::test]
async fn task_get_accepts_number_as_string() {
    let tools = hub();
    let created = call(
        &tools,
        "task_create",
        json!({ "title": "пин", "body": "## Постановка\n\nтело" }),
    )
    .await
    .expect("create");
    let number: u64 = created
        .trim_start_matches("task #")
        .split(' ')
        .next()
        .unwrap()
        .parse()
        .expect("number");
    let text = call(&tools, "task_get", json!({ "number": number.to_string() }))
        .await
        .expect("get строкой");
    assert!(text.starts_with(&format!("#{number} пин — Analys")));
    assert!(text.contains("body_hash: "));
}

/// Метка времени коммента в выводе task_get (#1432): строка после «---»
/// — ISO 8601 UTC ровно формата YYYY-MM-DDTHH:MM:SSZ (контракт
/// тайминг-разборов дримера); тело коммента следует за меткой.
#[tokio::test]
async fn task_get_comment_carries_iso_utc_stamp() {
    let tools = hub();
    let created = call(
        &tools,
        "task_create",
        json!({ "title": "пин метки", "body": "## Постановка\n\nтело" }),
    )
    .await
    .expect("create");
    let number: u64 = created
        .trim_start_matches("task #")
        .split(' ')
        .next()
        .unwrap()
        .parse()
        .expect("number");
    call(
        &tools,
        "task_comment",
        json!({ "number": number, "text": "вербатим" }),
    )
    .await
    .expect("comment");
    let text = call(&tools, "task_get", json!({ "number": number }))
        .await
        .expect("get");
    let block = text
        .split("--- comments (latest 1 of 1) ---\n")
        .nth(1)
        .expect("хвост комментов");
    let stamp = block
        .trim_start_matches("---\n")
        .lines()
        .next()
        .expect("строка метки");
    assert_eq!(stamp.len(), 20, "метка YYYY-MM-DDTHH:MM:SSZ ровно: {stamp}");
    let separators = [
        (4, '-'),
        (7, '-'),
        (10, 'T'),
        (13, ':'),
        (16, ':'),
        (19, 'Z'),
    ];
    for (i, ch) in stamp.chars().enumerate() {
        match separators.iter().find(|(pos, _)| *pos == i) {
            Some((_, expected)) => assert_eq!(ch, *expected, "позиция {i} в «{stamp}»"),
            None => assert!(ch.is_ascii_digit(), "позиция {i} в «{stamp}» — не цифра"),
        }
    }
    let body_line = block
        .trim_start_matches("---\n")
        .lines()
        .nth(1)
        .expect("тело за меткой");
    assert_eq!(body_line, "вербатим");
}

#[tokio::test]
async fn task_create_rejects_unknown_type_with_canonical_list() {
    let tools = hub();
    let err = call(
        &tools,
        "task_create",
        json!({ "title": "пин", "type": "супер" }),
    )
    .await
    .expect_err("отказ");
    assert_eq!(
        err,
        "unknown type «супер»: bug|spike|docs|other|feature|refactoring|legacy|qa|infrastructure"
    );
}

#[tokio::test]
async fn session_export_requires_ses_prefix() {
    let tools = hub();
    let err = call(&tools, "session_export", json!({ "session": "мусор" }))
        .await
        .expect_err("отказ");
    assert_eq!(err, "session must be ses_…");
}

#[tokio::test]
async fn task_body_section_requires_base_hash() {
    let tools = hub();
    let err = call(
        &tools,
        "task_body_section",
        json!({ "number": 1, "section": "Постановка", "content": "x" }),
    )
    .await
    .expect_err("отказ");
    assert_eq!(
        err,
        "base_hash is required (body_hash from the fm of task_get)"
    );
}

/// Ветка `_` match-таблицы диспетчера: неизвестное имя — громкий отказ.
#[tokio::test]
async fn unknown_tool_is_loud_at_dispatcher() {
    let tools = hub();
    let err = call(&tools, "нет_такого_тула", json!({}))
        .await
        .expect_err("отказ");
    assert_eq!(err, "unknown tool: нет_такого_тула");
}

/// Главный сценарий через живой диспетчер: create → квитанция Analys,
/// get → fm с body_hash и телом; task_list показывает строку.
#[tokio::test]
async fn create_get_list_roundtrip_reports_analys_receipt() {
    let tools = hub();
    let created = call(
        &tools,
        "task_create",
        json!({ "title": "круговерть", "body": "## Постановка\n\nтело" }),
    )
    .await
    .expect("create");
    assert_eq!(created, "task #1 created (Analys) on the uwuwu board");
    let get = call(&tools, "task_get", json!({ "number": 1 }))
        .await
        .expect("get");
    assert!(get.starts_with("#1 круговерть — Analys"));
    assert!(get.contains("body_hash: "));
    assert!(get.contains("## Постановка"));
    let list = call(&tools, "task_list", json!({})).await.expect("list");
    assert!(list.contains("круговерть"));
    assert!(list.contains("Analys"));
}

// ---- Пины статуса Wait на живом диспетчере (#1441) ----
// (tester #1383: пины восстановлены из master — ребейз унёс файл целиком
// по --theirs, и четыре пина #1441 выпали из контура.)

/// task_edit ставит Wait, чтение возвращает канон Wait (create → edit →
/// get → list на живом диспетчере, не юнит).
#[tokio::test]
async fn status_wait_edit_reads_back_canon_wait() {
    let tools = hub();
    let created = call(
        &tools,
        "task_create",
        json!({ "title": "wait-прогон", "direct": true }),
    )
    .await
    .expect("create direct");
    assert_eq!(
        created,
        "direct task #1 created (In Progress) on the uwuwu board"
    );
    let edited = call(
        &tools,
        "task_edit",
        json!({ "number": 1, "status": "Wait" }),
    )
    .await
    .expect("edit Wait");
    assert_eq!(edited, "task #1 updated, carrier confirms: \"Wait\"");
    let get = call(&tools, "task_get", json!({ "number": 1 }))
        .await
        .expect("get");
    assert!(get.starts_with("#1 wait-прогон — Wait"));
    let list = call(&tools, "task_list", json!({})).await.expect("list");
    assert!(list.contains("Wait"), "строка в Wait видна в списке");
}

/// Алиас «Вейт» — мост совместимости (#1441): парсится в Wait и ставит
/// канон-опцию Wait идемпотентно (повторная установка не отказ).
#[tokio::test]
async fn status_weit_alias_sets_canon_wait() {
    let tools = hub();
    call(
        &tools,
        "task_create",
        json!({ "title": "wait-алиас", "direct": true }),
    )
    .await
    .expect("create direct");
    let edited = call(
        &tools,
        "task_edit",
        json!({ "number": 1, "status": "Вейт" }),
    )
    .await
    .expect("edit алиасом");
    assert_eq!(edited, "task #1 updated, carrier confirms: \"Wait\"");
    let get = call(&tools, "task_get", json!({ "number": 1 }))
        .await
        .expect("get");
    assert!(
        get.starts_with("#1 wait-алиас — Wait"),
        "алиас ставит канон Wait"
    );
}

/// Нераспознанный статус — громкий отказ гарда канона, состояние строки
/// не меняется (Empty не ставится молча).
#[tokio::test]
async fn status_garbage_is_loud_guard_reject() {
    let tools = hub();
    call(
        &tools,
        "task_create",
        json!({ "title": "wait-мусор", "direct": true }),
    )
    .await
    .expect("create direct");
    call(
        &tools,
        "task_edit",
        json!({ "number": 1, "status": "Wait" }),
    )
    .await
    .expect("edit Wait");
    let err = call(
        &tools,
        "task_edit",
        json!({ "number": 1, "status": "Ерунда" }),
    )
    .await
    .expect_err("отказ");
    assert_eq!(
        err,
        "board: transition Wait → Empty outside the pipeline canon"
    );
    let get = call(&tools, "task_get", json!({ "number": 1 }))
        .await
        .expect("get");
    assert!(
        get.starts_with("#1 wait-мусор — Wait"),
        "статус не изменился"
    );
}

/// Контракт тула task_edit (#1441): строка канона статусов несёт «Wait»,
/// старое имя «Вейт» из описания убрано — агент видит новый канон.
#[tokio::test]
async fn task_edit_descriptor_carries_wait_canon() {
    let desc = super::tools::tool_descriptors();
    let edit = desc
        .as_array()
        .expect("дескрипторы — массив")
        .iter()
        .find(|t| t["name"] == "task_edit")
        .expect("task_edit в дескрипторах");
    let status = edit["inputSchema"]["properties"]["status"]["description"]
        .as_str()
        .expect("status description");
    assert!(status.contains("/Wait"), "канон в описании тула: {status}");
    assert!(!status.contains("Вейт"), "старое имя не в контракте тула");
}

/// Пины rename статуса канбана (#1447): happy-path на MemoryHub-носителе
/// (строка Done не задета — распределение живо), отказы до мутации —
/// точные тексты (агент по ним различает ветки повтора/коллизии/no-op).
#[tokio::test]
async fn board_status_rename_happy_reports_journal_and_histogram() {
    let tools = hub();
    let answer = call(
        &tools,
        "board_status_rename",
        json!({ "from": "Deleted", "to": "Trash" }),
    )
    .await
    .expect("rename");
    assert!(
        answer.contains("Renamed \"Deleted\" → \"Trash\""),
        "{answer}"
    );
    assert!(answer.contains("option id kept: mopt-deleted"), "{answer}");
    assert!(
        answer.contains("Pre-image journal: /memory/rename-pre-image.json"),
        "{answer}"
    );
    assert!(answer.contains("Histogram before"), "{answer}");
    // не-каноническое имя — warn, не отказ (стоп-краны не заводим)
    assert!(
        answer.contains("WARNING: new name \"Trash\" is not a canonical"),
        "{answer}"
    );
}

#[tokio::test]
async fn board_status_rename_unknown_from_is_loud_and_hints_retry() {
    let tools = hub();
    let err = call(
        &tools,
        "board_status_rename",
        json!({ "from": "Несуществующий", "to": "Trash" }),
    )
    .await
    .expect_err("отказ");
    assert!(err.contains("not found"), "{err}");
    assert!(
        err.contains("already renamed"),
        "подсказка про повтор: {err}"
    );
    assert!(err.contains("pre-image journal"), "{err}");
}

#[tokio::test]
async fn board_status_rename_to_equals_from_refuses() {
    let tools = hub();
    let err = call(
        &tools,
        "board_status_rename",
        json!({ "from": "Draft", "to": "Draft" }),
    )
    .await
    .expect_err("отказ");
    assert!(err.contains("no-op"), "{err}");
}

#[tokio::test]
async fn board_status_rename_collision_refuses() {
    let tools = hub();
    let err = call(
        &tools,
        "board_status_rename",
        json!({ "from": "Draft", "to": "Done" }),
    )
    .await
    .expect_err("отказ");
    assert!(err.contains("collision"), "{err}");
}

#[tokio::test]
async fn board_status_rename_missing_args_keep_exact_text() {
    let tools = hub();
    let err = call(&tools, "board_status_rename", json!({}))
        .await
        .expect_err("отказ");
    assert_eq!(err, "from is required (current option name)");
    let err = call(&tools, "board_status_rename", json!({ "from": "Draft" }))
        .await
        .expect_err("отказ");
    assert_eq!(err, "to is required (new option name)");
}

#[tokio::test]
async fn board_status_rename_project_outside_config_refuses() {
    let tools = hub();
    let err = call(
        &tools,
        "board_status_rename",
        json!({ "from": "Draft", "to": "Trash", "project": "origa" }),
    )
    .await
    .expect_err("отказ");
    assert!(err.contains("not in config"), "{err}");
}

// ---- Форма квитанции Unverified на поверхности MCP (тест-ревью C2 #1437) ----

/// Причина фейка — каноничный текст Unverified из hub.rs: rate limit на
/// verify-риде (единственная ветка, где задача принята, а применения
/// свежие чтения не показали).
const UNVERIFIED_REASON: &str = "GraphQL rate limit — confirmation not performed";

/// Фейк доски пина: всё делегирует MemoryHub, кроме set_status — запись
/// ПРИМЕНЯЕТСЯ (носитель сдвинулся, write-through жив — как PATCH на
/// бою), а исход отвечает Unverified. Точная имитация rate-limit ветки
/// verify (#1437): GitHub принял, квитанция успеха не заявляет.
struct UnverifiedStatusBoard {
    inner: Arc<crate::mockhub::MemoryHub>,
}

#[async_trait::async_trait]
impl uwuwu_usecase::TaskBoardRepo for UnverifiedStatusBoard {
    async fn read_board(
        &self,
        board: &str,
    ) -> uwuwu_usecase::UsecaseResult<Vec<uwuwu_domain::Task>> {
        self.inner.read_board(board).await
    }

    async fn set_status(
        &self,
        board: &str,
        number: u64,
        status: uwuwu_domain::Status,
    ) -> uwuwu_usecase::UsecaseResult<uwuwu_usecase::StatusDelivery> {
        self.inner.set_status(board, number, status).await?;
        Ok(uwuwu_usecase::StatusDelivery::Unverified {
            reason: UNVERIFIED_REASON.into(),
        })
    }

    async fn row_status(
        &self,
        board: &str,
        number: u64,
    ) -> uwuwu_usecase::UsecaseResult<Option<uwuwu_domain::Status>> {
        self.inner.row_status(board, number).await
    }

    async fn row_exists(&self, board: &str, number: u64) -> uwuwu_usecase::UsecaseResult<bool> {
        self.inner.row_exists(board, number).await
    }

    async fn set_session(
        &self,
        board: &str,
        number: u64,
        slot: uwuwu_domain::Slot,
        session: &str,
    ) -> uwuwu_usecase::UsecaseResult<()> {
        self.inner.set_session(board, number, slot, session).await
    }

    async fn set_branch(
        &self,
        board: &str,
        number: u64,
        branch: &uwuwu_domain::BranchName,
    ) -> uwuwu_usecase::UsecaseResult<()> {
        self.inner.set_branch(board, number, branch).await
    }
    async fn set_pr(
        &self,
        board: &str,
        number: u64,
        pr_url: &uwuwu_domain::PrUrl,
    ) -> uwuwu_usecase::UsecaseResult<()> {
        self.inner.set_pr(board, number, pr_url).await
    }

    async fn set_board_cells(
        &self,
        board: &str,
        number: u64,
        priority: Option<&str>,
        automerge: Option<bool>,
        direct: Option<bool>,
        fact_track: Option<bool>,
    ) -> uwuwu_usecase::UsecaseResult<()> {
        self.inner
            .set_board_cells(board, number, priority, automerge, direct, fact_track)
            .await
    }

    async fn clear_session(
        &self,
        board: &str,
        number: u64,
        slot: uwuwu_domain::Slot,
    ) -> uwuwu_usecase::UsecaseResult<()> {
        self.inner.clear_session(board, number, slot).await
    }

    async fn post_comment(&self, number: u64, body: &str) -> uwuwu_usecase::UsecaseResult<String> {
        self.inner.post_comment(number, body).await
    }

    async fn close_issue(&self, number: u64) -> uwuwu_usecase::UsecaseResult<()> {
        self.inner.close_issue(number).await
    }

    async fn latest_comments(
        &self,
        number: u64,
    ) -> uwuwu_usecase::UsecaseResult<Vec<uwuwu_usecase::CommentRef>> {
        self.inner.latest_comments(number).await
    }

    async fn read_issue(
        &self,
        board: &str,
        number: u64,
    ) -> uwuwu_usecase::UsecaseResult<uwuwu_usecase::IssueView> {
        self.inner.read_issue(board, number).await
    }

    async fn issue_fields(
        &self,
        number: u64,
    ) -> uwuwu_usecase::UsecaseResult<uwuwu_domain::TaskFields> {
        self.inner.issue_fields(number).await
    }

    async fn create_issue(
        &self,
        board: &str,
        project: &str,
        title: &str,
        body: &str,
        kind_name: Option<&str>,
    ) -> uwuwu_usecase::UsecaseResult<u64> {
        self.inner
            .create_issue(board, project, title, body, kind_name)
            .await
    }

    async fn set_issue_type(
        &self,
        number: u64,
        kind_name: &str,
    ) -> uwuwu_usecase::UsecaseResult<()> {
        self.inner.set_issue_type(number, kind_name).await
    }

    async fn rename_issue(&self, number: u64, title: &str) -> uwuwu_usecase::UsecaseResult<()> {
        self.inner.rename_issue(number, title).await
    }

    async fn patch_cells(
        &self,
        number: u64,
        mutate: uwuwu_usecase::FmMutator,
    ) -> uwuwu_usecase::UsecaseResult<()> {
        self.inner.patch_cells(number, mutate).await
    }

    async fn close_issue_ext(
        &self,
        number: u64,
        not_planned: bool,
    ) -> uwuwu_usecase::UsecaseResult<()> {
        self.inner.close_issue_ext(number, not_planned).await
    }

    async fn archive_row(&self, board: &str, number: u64) -> uwuwu_usecase::UsecaseResult<()> {
        self.inner.archive_row(board, number).await
    }

    async fn issues_with_title(&self, title: &str) -> uwuwu_usecase::UsecaseResult<Vec<u64>> {
        self.inner.issues_with_title(title).await
    }

    async fn replace_body(&self, number: u64, body_md: &str) -> uwuwu_usecase::UsecaseResult<()> {
        self.inner.replace_body(number, body_md).await
    }
}

/// Диспетчер на подменённой доске: сборка фабрики та же, writer/tasks
/// смотрят на UnverifiedStatusBoard; aux — та же память, что под фейком
/// (носитель, куда фейк применяет запись).
fn hub_unverified() -> Arc<ToolHub> {
    let memory = Arc::new(crate::mockhub::MemoryHub::from_env());
    let board: Arc<dyn uwuwu_usecase::TaskBoardRepo> = Arc::new(UnverifiedStatusBoard {
        inner: memory.clone(),
    });
    let (tools, _aux) = super::test_support::tool_hub_on_board(
        Arc::new(crate::mockhub::MemoryHub::from_env()),
        blob_usecase(),
        Arc::new(StubCronState::default()),
        board,
        memory,
    );
    tools
}

/// Форма квитанции Unverified (тест-ревью C2 #1437): статусная запись
/// принята GitHub (носитель сдвинулся), свежее чтение применение не
/// подтвердило (rate limit на verify-риде). Ответ MCP НЕ заявляет
/// success: «did NOT confirm» + предписание контрольного чтения
/// НОСИТЕЛЯ, слепой ретрай осуждён. Сквозь настоящий TaskWriteUseCase
/// (исход проходит write.rs насквозь до текста тула). Второй смысл
/// пина: «accepted, not confirmed» ≠ «не применено» — носитель несёт
/// записанный статус.
#[tokio::test]
async fn task_edit_status_unverified_receipt_does_not_claim_success() {
    let tools = hub_unverified();
    let created = call(
        &tools,
        "task_create",
        json!({ "title": "unverified-прогон", "direct": true }),
    )
    .await
    .expect("create direct");
    assert_eq!(
        created,
        "direct task #1 created (In Progress) on the uwuwu board"
    );
    let edited = call(
        &tools,
        "task_edit",
        json!({ "number": 1, "status": "Wait" }),
    )
    .await
    .expect("edit Wait — исход Unverified не Err, задача принята");
    assert_eq!(
        edited,
        format!(
            "task #1: status accepted by GitHub, carrier read did NOT confirm \
             ({UNVERIFIED_REASON}) — verify the CARRIER (gh, board) yourself, do not retry blindly"
        )
    );
    // write-through жив: носитель (внутренность фейка) несёт Wait —
    // «accepted, not confirmed» ≠ «не применено»; task_get идёт через
    // TaskView → фейк → тот же носитель, куда писала запись
    let get = call(&tools, "task_get", json!({ "number": 1 }))
        .await
        .expect("get");
    assert!(
        get.starts_with("#1 unverified-прогон — Wait"),
        "запись применена несмотря на Unverified: {get}"
    );
}

// --- docs_delete (#1658): квитанция, гард, бэкап ---

#[tokio::test]
async fn docs_delete_reports_backup_and_removes_live_doc() {
    let tools = hub();
    call(
        &tools,
        "docs_write",
        json!({ "slug": "Projects/uwuwu/Удаляемый", "content": "# док\n" }),
    )
    .await
    .expect("seed");

    let answer = call(
        &tools,
        "docs_delete",
        json!({ "slug": "Projects/uwuwu/Удаляемый" }),
    )
    .await
    .expect("delete");

    assert!(
        answer.starts_with("deleted: «Projects/uwuwu/Удаляемый»"),
        "{answer}"
    );
    assert!(answer.contains("blob sha "), "{answer}");
    assert!(answer.contains("commit "), "{answer}");
    assert!(answer.contains("\nbackup: blobs/uwuwu/misc/"), "{answer}");
    assert!(answer.contains("TTL 7 days"), "{answer}");
    assert!(
        answer.contains("\npost-check: absent in tree at "),
        "{answer}"
    );
    let ls = call(&tools, "docs_ls", json!({ "path": "Projects/uwuwu" }))
        .await
        .expect("ls");
    assert!(!ls.contains("Удаляемый"), "док остался в листинге: {ls}");
}

#[tokio::test]
async fn docs_delete_absent_doc_is_not_silent_success() {
    let tools = hub();

    let error = call(
        &tools,
        "docs_delete",
        json!({ "slug": "Projects/uwuwu/Нет" }),
    )
    .await
    .expect_err("absent");

    assert_eq!(
        error,
        "absent: doc «Projects/uwuwu/Нет» does not exist (no backup, no mutation)"
    );
}

#[tokio::test]
async fn docs_delete_refuses_outside_wiki_roots() {
    let tools = hub();

    let error = call(&tools, "docs_delete", json!({ "slug": "cli/x" }))
        .await
        .expect_err("root-prefix");

    assert!(error.contains("refused: root-prefix"), "{error}");
}

#[tokio::test]
async fn docs_delete_keeps_suffix_twin_as_information() {
    let tools = hub();
    for slug in ["Projects/foo", "Experience/uwuwu/Projects/foo"] {
        call(
            &tools,
            "docs_write",
            json!({ "slug": slug, "content": "# x\n" }),
        )
        .await
        .expect("seed");
    }

    let answer = call(&tools, "docs_delete", json!({ "slug": "Projects/foo" }))
        .await
        .expect("delete");

    assert!(answer.starts_with("deleted: «Projects/foo»"), "{answer}");
    assert!(
        answer.contains("suffix twins: Experience/uwuwu/Projects/foo"),
        "{answer}"
    );
    let ls = call(&tools, "docs_ls", json!({ "path": "Experience/uwuwu" }))
        .await
        .expect("ls");
    assert!(ls.contains("Projects/foo"), "тёзка пострадала: {ls}");
}

#[tokio::test]
async fn docs_delete_without_backup_refuses_before_mutation() {
    let (tools, _) = tool_hub(
        Arc::new(crate::mockhub::MemoryHub::from_env()),
        blob_usecase_missing(&["UWUWU_S3_BUCKET"]),
        Arc::new(StubCronState::default()),
    );
    call(
        &tools,
        "docs_write",
        json!({ "slug": "Projects/uwuwu/БезБэкапа", "content": "# док\n" }),
    )
    .await
    .expect("seed");

    let error = call(
        &tools,
        "docs_delete",
        json!({ "slug": "Projects/uwuwu/БезБэкапа" }),
    )
    .await
    .expect_err("backup-failed");

    assert!(error.contains("refused: backup-failed"), "{error}");
    let doc = call(
        &tools,
        "docs_read",
        json!({ "slug": "Projects/uwuwu/БезБэкапа" }),
    )
    .await
    .expect("док цел");
    assert!(doc.contains("# док"), "док тронут при отказе бэкапа: {doc}");
}

#[tokio::test]
async fn docs_delete_refusal_is_error_over_mcp_wire() {
    let tools = hub();
    let state = Arc::new(super::McpState {
        bearer: Arc::new("t-bearer".into()),
        tools,
    });
    let app = super::mcp_router(state);

    let answer = mcp_call(&app, "t-bearer", "docs_delete", json!({ "slug": "cli/x" })).await;

    assert_eq!(answer["result"]["isError"], Value::Bool(true), "{answer}");
    let text = answer["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(text.contains("refused: root-prefix"), "{text}");
}

#[tokio::test]
async fn docs_delete_refuses_unknown_mutation_argument() {
    let tools = hub();
    let error = call(
        &tools,
        "docs_delete",
        json!({ "slug": "Projects/foo", "content": "x" }),
    )
    .await
    .expect_err("unknown arg");
    assert!(error.contains("unknown argument(s) content"), "{error}");
}

/// Пин тестировщика (#1658, uwuwu#1658 UC-1): слаг прогоняется тем же
/// sanitize_slug, что и у чтения/записи — «docs/…md» сносит тот же док.
#[tokio::test]
async fn docs_delete_normalizes_docs_prefix_and_md_suffix() {
    let tools = hub();
    let slug = "Projects/uwuwu/Нормализация-1658";
    call(
        &tools,
        "docs_write",
        json!({ "slug": slug, "content": "# док\n" }),
    )
    .await
    .expect("seed");

    let answer = call(
        &tools,
        "docs_delete",
        json!({ "slug": format!("docs/{slug}.md") }),
    )
    .await
    .expect("delete");

    assert!(
        answer.starts_with("deleted: «Projects/uwuwu/Нормализация-1658»"),
        "{answer}"
    );
    let error = call(&tools, "docs_read", json!({ "slug": slug }))
        .await
        .expect_err("док должен быть снесён");
    assert!(error.contains("not found"), "{error}");
}

/// Ветка absent обязана называть живого длинного тёзку и не трогать его
/// (#1658, uwuwu#1658 NFT-1): кандидаты — не отказ, но и не тихий успех.
#[tokio::test]
async fn docs_delete_absent_names_suffix_candidates_and_keeps_twin() {
    let tools = hub();
    let twin = "Experience/uwuwu/Projects/Отсутствующий-1658";
    call(
        &tools,
        "docs_write",
        json!({ "slug": twin, "content": "# тёзка\n" }),
    )
    .await
    .expect("seed twin");

    let error = call(
        &tools,
        "docs_delete",
        json!({ "slug": "Projects/Отсутствующий-1658" }),
    )
    .await
    .expect_err("absent");

    assert!(error.starts_with("absent:"), "{error}");
    assert!(
        error.contains("suffix candidates: Experience/uwuwu/Projects/Отсутствующий-1658"),
        "{error}"
    );
    let ls = call(&tools, "docs_ls", json!({ "path": "Experience/uwuwu" }))
        .await
        .expect("ls");
    assert!(
        ls.contains("Projects/Отсутствующий-1658"),
        "тёзка пострадала при absent: {ls}"
    );
}

/// Успех delete на проводе MCP: isError=false и текст ветки deleted —
/// зеркало отказа, который уже закреплён в docs_delete_refusal_is_error_over_mcp_wire.
#[tokio::test]
async fn docs_delete_success_is_not_error_over_mcp_wire() {
    let tools = hub();
    let slug = "Projects/uwuwu/Проводимый";
    call(
        &tools,
        "docs_write",
        json!({ "slug": slug, "content": "# док\n" }),
    )
    .await
    .expect("seed");
    let state = Arc::new(super::McpState {
        bearer: Arc::new("t-bearer".into()),
        tools,
    });
    let app = super::mcp_router(state);

    let answer = mcp_call(&app, "t-bearer", "docs_delete", json!({ "slug": slug })).await;

    assert_eq!(answer["result"]["isError"], Value::Bool(false), "{answer}");
    let text = answer["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(
        text.starts_with("deleted: «Projects/uwuwu/Проводимый»"),
        "{text}"
    );
}

// --- #1697: явная деградация слаг-гарда на четырёх сайтах -----------------

use crate::rename_journal::test_support::{assert_event, trap_events};

/// Ловушка событий — thread-local dispatcher; при параллельном прогоне
/// соседние тесты оставляют на том же потоке чужой dispatcher. Сериализуем
/// пины: один активный trap на прогон. Все тесты, которые порождают это
/// событие, обязаны брать trap — иначе callsite кэширует «не интересно»
/// до первого подписчика и последующие ассерты видят пустой лог.
static TRAP_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn scoped_trap() -> (
    std::sync::MutexGuard<'static, ()>,
    tracing::dispatcher::DefaultGuard,
    std::sync::Arc<std::sync::Mutex<Vec<String>>>,
) {
    let serial = TRAP_SERIAL.lock().expect("trap serial");
    let (guard, records) = trap_events();
    (serial, guard, records)
}

#[tokio::test]
async fn docs_write_degraded_tree_logs_slug_guard_event() {
    let (_serial, _guard, records) = scoped_trap();
    let (tools, _) = tool_hub(
        Arc::new(TreeFailingWiki),
        blob_usecase(),
        Arc::new(StubCronState::default()),
    );
    let answer = call(
        &tools,
        "docs_write",
        json!({ "slug": "Experience/platform/degraded", "content": "# x\n" }),
    )
    .await
    .expect("запись в шторм не блокируется");
    assert!(
        answer.contains("collision checks skipped (tree unavailable)"),
        "{answer}"
    );
    let events = records.lock().expect("trap lock");
    assert_event(&events, "slug_guard.degraded");
}

#[tokio::test]
async fn docs_write_read_failure_with_fm_logs_its_own_reason() {
    let (_serial, _guard, records) = scoped_trap();
    let (tools, _) = tool_hub(
        Arc::new(ReadFailingWiki),
        blob_usecase(),
        Arc::new(StubCronState::default()),
    );
    let answer = call(
        &tools,
        "docs_write",
        json!({ "slug": "Experience/degraded-read", "content": "---\ntitle: x\n---\n\nтело\n" }),
    )
    .await
    .expect("вход с fm не зависит от сбоя чтения");
    assert!(
        answer.contains("collision checks skipped (doc read failed)"),
        "причина деградации не та: {answer}"
    );
    let events = records.lock().expect("trap lock");
    assert_event(&events, "slug_guard.degraded");
}

#[tokio::test]
async fn docs_append_degraded_tree_logs_slug_guard_event() {
    let (_serial, _guard, records) = scoped_trap();
    let (tools, _) = tool_hub(
        Arc::new(TreeFailingWiki),
        blob_usecase(),
        Arc::new(StubCronState::default()),
    );
    let err = call(
        &tools,
        "docs_append",
        json!({ "slug": "cli/x", "line": "строка" }),
    )
    .await
    .expect_err("append-nonexistent");
    assert!(err.contains("append-nonexistent"), "{err}");
    let events = records.lock().expect("trap lock");
    assert!(
        events.iter().any(|e| e == "slug_guard.degraded"),
        "нет slug_guard.degraded: {events:?}; err={err}"
    );
}

#[tokio::test]
async fn docs_delete_degraded_tree_refuses_and_logs() {
    let (_serial, _guard, records) = scoped_trap();
    let (tools, _) = tool_hub(
        Arc::new(TreeFailingWiki),
        blob_usecase(),
        Arc::new(StubCronState::default()),
    );
    let err = call(
        &tools,
        "docs_delete",
        json!({ "slug": "Projects/uwuwu/degraded" }),
    )
    .await
    .expect_err("fail-closed при мёртвом дереве");
    assert!(
        err.contains("precheck-unavailable"),
        "вердикт предпроверки не назван: {err}"
    );
    assert!(
        err.contains("collision checks skipped (tree unavailable)"),
        "деградация гарда не названа: {err}"
    );
    let events = records.lock().expect("trap lock");
    assert_event(&events, "slug_guard.degraded");
}

/// Отказы удаления: стаб возвращает нужный вариант ошибки, пин проверяет
/// только оформление текста — поведение адаптера закреплено в срезе 1.
struct DeleteFailingWiki {
    error: uwuwu_usecase::UsecaseError,
}

#[async_trait::async_trait]
impl uwuwu_usecase::WikiRepo for DeleteFailingWiki {
    async fn read_doc(&self, _slug: &str) -> uwuwu_usecase::UsecaseResult<Option<String>> {
        Ok(Some("# doc\n".into()))
    }
    async fn list_docs(&self, _prefix: &str) -> uwuwu_usecase::UsecaseResult<Vec<String>> {
        Ok(Vec::new())
    }
    async fn doc_shas(&self) -> uwuwu_usecase::UsecaseResult<Vec<(String, String)>> {
        Ok(Vec::new())
    }
    async fn write_doc(&self, _: &str, _: &str, _: &str) -> uwuwu_usecase::UsecaseResult<()> {
        Ok(())
    }
    async fn append_doc(&self, _: &str, _: &str, _: &str) -> uwuwu_usecase::UsecaseResult<usize> {
        Ok(0)
    }
    async fn read_doc_with_sha(
        &self,
        _slug: &str,
    ) -> uwuwu_usecase::UsecaseResult<Option<uwuwu_usecase::DocSnapshot>> {
        Ok(Some(uwuwu_usecase::DocSnapshot {
            content: "# doc\n".into(),
            blob_sha: "A".into(),
        }))
    }
    async fn delete_doc(
        &self,
        _slug: &str,
        _old_blob_sha: &str,
        _message: &str,
    ) -> uwuwu_usecase::UsecaseResult<uwuwu_usecase::DocDeleteCommit> {
        Err(self.error.clone())
    }
}

async fn delete_error_text(error: uwuwu_usecase::UsecaseError) -> String {
    let (tools, _) = tool_hub(
        Arc::new(DeleteFailingWiki { error }),
        blob_usecase(),
        Arc::new(StubCronState::default()),
    );
    call(
        &tools,
        "docs_delete",
        json!({ "slug": "Projects/uwuwu/texts" }),
    )
    .await
    .expect_err("отказ")
}

#[tokio::test]
async fn delete_refusal_texts_keep_backup_scope_honest() {
    use uwuwu_usecase::UsecaseError;

    let race = delete_error_text(UsecaseError::RaceRef("blob changed".into())).await;
    assert!(race.contains("any backup is for the read version only"), "{race}");

    let absent = delete_error_text(UsecaseError::DocAbsentAtRef(
        "already absent at ref; no delete was committed, backup of the read version only".into(),
    ))
    .await;
    assert!(!absent.contains("no backup"), "{absent}");
    assert!(absent.contains("backup of the read version only"), "{absent}");

    let precheck = delete_error_text(UsecaseError::PrecheckUnavailable("tree: HTTP 502".into())).await;
    assert!(precheck.contains("no delete was committed"), "{precheck}");
    assert!(precheck.contains("HTTP 502"), "{precheck}");

    let board = delete_error_text(UsecaseError::Board("transport down".into())).await;
    assert!(
        !board.contains("no delete was committed") && !board.contains("вики не изменена"),
        "нейтральный catch-all обещает слишком много: {board}"
    );
}

#[test]
fn docs_descriptors_declare_slug_guard_degradation() {
    let list = super::tools::tool_descriptors();
    let description = |name: &str| -> String {
        list.as_array()
            .expect("список тулов")
            .iter()
            .find(|t| t["name"].as_str() == Some(name))
            .expect("тул на месте")["description"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    };
    for (name, marker) in [
        ("docs_write", "collision checks are skipped"),
        ("docs_delete", "refused before mutation"),
    ] {
        let text = description(name);
        assert!(
            text.contains(marker),
            "дескриптор {name} не оговаривает деградацию: {text}"
        );
    }
}
