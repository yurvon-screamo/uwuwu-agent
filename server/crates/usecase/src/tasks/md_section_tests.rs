//! Юнит-матрица секционной хирургии (порт легаси sy_dispatch-тестов,
//! фикс-раунд aa28614, переработка под пин-семантику #1152): перенесённые
//! bare/h2-кейсы, переписанные под пин, новые пин-кейсы, body_hash.

use super::md_section::{
    body_hash, heading_of, normalize_heading_query, replace_md_section, toggles_fence, SectionEdit,
};

// ——— примитивы md_heading (порт без изменений) ———

#[test]
fn heading_requires_space_after_hashes() {
    assert_eq!(heading_of("#bug"), None);
    assert_eq!(heading_of("##x"), None);
}

#[test]
fn heading_detects_level_and_text() {
    assert_eq!(heading_of("# Титул"), Some((1, "Титул".to_string())));
    assert_eq!(
        heading_of("### Подсекция"),
        Some((3, "Подсекция".to_string()))
    );
}

#[test]
fn heading_rejects_seven_hashes_and_empty_text() {
    assert_eq!(heading_of("####### x"), None);
    assert_eq!(heading_of("## "), None);
}

#[test]
fn heading_normalizes_whitespace() {
    assert_eq!(
        heading_of("##  Двойные   пробелы "),
        Some((2, "Двойные пробелы".to_string()))
    );
}

#[test]
fn heading_strips_block_id_suffix() {
    assert_eq!(
        heading_of("## Причина ^20261104120000-ab12cd"),
        Some((2, "Причина".to_string()))
    );
}

#[test]
fn heading_keeps_caret_text_that_is_not_block_id() {
    assert_eq!(
        heading_of("## A ^не id!"),
        Some((2, "A ^не id!".to_string()))
    );
}

#[test]
fn fence_toggle_detection() {
    assert!(toggles_fence("```"));
    assert!(toggles_fence("```gherkin"));
    assert!(toggles_fence("  ```"));
    assert!(!toggles_fence("текст"));
    assert!(!toggles_fence("`"));
}

// ——— нормализация запроса: ATX-префикс пинит уровень (модификация порта) ———

#[test]
fn query_pin_from_atx_prefix() {
    assert_eq!(normalize_heading_query("## Решение"), (2, "Решение".into()));
    assert_eq!(
        normalize_heading_query("###  Подсекция ^20261105230000-ab12cd"),
        (3, "Подсекция".into())
    );
    assert_eq!(normalize_heading_query("# Титул"), (1, "Титул".into()));
    assert_eq!(
        normalize_heading_query("#### Глубоко"),
        (4, "Глубоко".into())
    );
}

#[test]
fn query_without_valid_atx_defaults_to_h2() {
    assert_eq!(normalize_heading_query(" Решение "), (2, "Решение".into()));
    assert_eq!(
        normalize_heading_query("#bug"),
        (2, "#bug".into()),
        "нет пробела после хэшей — не ATX, не пин"
    );
    assert_eq!(
        normalize_heading_query("####### x"),
        (2, "####### x".into()),
        "семь хэшей — не заголовок, не пин"
    );
}

// ——— замена h2 (перенесённые bare/h2-кейсы легаси) ———

#[test]
fn replaces_body_keeps_heading() {
    let md = "## А\nстарое\n\n## Б\nвторое\n";
    let (out, edit) = replace_md_section(md, "А", "новое содержимое\n").unwrap();
    assert!(out.contains("## А\nновое содержимое"));
    assert!(out.contains("## Б\nвторое"));
    assert_eq!(
        edit,
        SectionEdit::Replaced {
            duplicates_removed: 0
        }
    );
}

#[test]
fn appends_missing_section() {
    let md = "## А\nпервое\n";
    let (out, edit) = replace_md_section(md, "Новая", "контент").unwrap();
    assert!(out.contains("## Новая\nконтент"));
    assert!(out.contains("## А\nпервое"));
    assert_eq!(edit, SectionEdit::Appended);
}

#[test]
fn replaces_first_and_removes_duplicate_sections() {
    let md = "# Таск\n\n## Причина\nпервое вхождение\n\n## Причина\nвторое вхождение\n\n## Фикс\nправка\n";
    let (out, edit) = replace_md_section(md, "Причина", "новый механизм\n").unwrap();
    assert_eq!(out.matches("## Причина").count(), 1, "секция остаётся одна");
    assert!(
        !out.contains("первое вхождение"),
        "старый контент первого удалён"
    );
    assert!(!out.contains("второе вхождение"), "контент дубля удалён");
    assert!(
        out.contains("## Фикс\nправка"),
        "соседняя секция не тронута"
    );
    assert_eq!(
        edit,
        SectionEdit::Replaced {
            duplicates_removed: 1
        }
    );
}

#[test]
fn ends_section_at_higher_level_heading() {
    let md = "## А\nстарое\n# Заголовок H1\nхвост тайтла\n\n## Б\nвторое\n";
    let (out, _) = replace_md_section(md, "А", "новое\n").unwrap();
    assert!(out.contains("# Заголовок H1"), "h1 переживает replace");
    assert!(
        out.contains("хвост тайтла"),
        "контент после h1 переживает replace"
    );
    assert!(out.contains("## Б\nвторое"));
}

#[test]
fn h3_stays_inside_section() {
    let md = "## А\nввод\n### Подсекция\nдетали подсекции\n\n## Б\nвторое\n### Б-под\nб-детали\n";
    let (out, _) = replace_md_section(md, "А", "новое\n").unwrap();
    assert!(
        !out.contains("детали подсекции"),
        "h3 не рвёт диапазон: контент под h3 заменяется вместе с секцией"
    );
    assert!(
        out.contains("### Б-под\nб-детали"),
        "h3 чужой секции не тронут"
    );
    assert!(out.contains("## Б\nвторое"));
}

#[test]
fn normalizes_heading_and_argument() {
    let md = "##  Постановка\nстарое\n";
    let (out, edit) = replace_md_section(md, " Постановка ", "новое\n").unwrap();
    assert!(out.contains("новое"), "матч по нормализованному тексту");
    assert_eq!(out.matches("##").count(), 1, "без append-дубля секции");
    assert_eq!(
        edit,
        SectionEdit::Replaced {
            duplicates_removed: 0
        }
    );
}

#[test]
fn heading_preserved_byte_for_byte() {
    let md = "##  Причина ^20261104120000-ab12cd\nстарое\n\n## Б\nвторое\n";
    let (out, _) = replace_md_section(md, "Причина", "новое\n").unwrap();
    assert!(
        out.contains("##  Причина ^20261104120000-ab12cd\nновое"),
        "заматченный заголовок пишется исходной строкой (пробелы и ^id живы)"
    );
    assert_eq!(out.matches("##").count(), 2, "без append-дубля");
}

#[test]
fn matches_heading_with_block_id_suffix() {
    let md = "## Причина ^20261104120000-ab12cd\nстарое\n\n## Б\nвторое\n";
    let (out, edit) = replace_md_section(md, "Причина", "новое\n").unwrap();
    assert!(out.contains("новое"), "суффикс ^id не мешает матчу");
    assert_eq!(
        edit,
        SectionEdit::Replaced {
            duplicates_removed: 0
        }
    );
}

#[test]
fn fenced_h2_does_not_bound_section() {
    let md = "## А\nстарое\n```md\n## Фейк внутри фенса\n```\nхвост секции\n\n## Б\nвторое\n";
    let (out, _) = replace_md_section(md, "А", "новое\n").unwrap();
    assert!(
        !out.contains("Фейк"),
        "h2 внутри фенса — контент секции, заменяется"
    );
    assert!(!out.contains("хвост секции"), "секция длится сквозь фенс");
    assert!(out.contains("## Б\nвторое"));
}

#[test]
fn hash_line_inside_fence_survives() {
    let md = "## Сценарий (BDD)\nстарое\n```gherkin\nСценарий: x\n  Дано y\n  Тогда z  # факт\n```\n\n## Б\nвторое\n";
    let (out, _) = replace_md_section(md, "Сценарий (BDD)", "новое\n").unwrap();
    assert!(
        out.contains("## Б\nвторое"),
        "граница секции — настоящий заголовок Б"
    );
    assert_eq!(out.matches("## Б").count(), 1, "секция Б не задвоена");
    assert!(
        !out.contains("# факт"),
        "старый контент секции заменён целиком"
    );
}

#[test]
fn empty_content_clears_section() {
    let md = "## А\nстарое\n\n## Б\nвторое\n";
    let (out, _) = replace_md_section(md, "А", "").unwrap();
    assert!(out.contains("## А"), "заголовок сохранён");
    assert!(!out.contains("старое"), "контент очищен пустым вводом");
    assert!(out.contains("## Б\nвторое"));
}

#[test]
fn blank_line_restored_before_next_heading() {
    let md = "## А\nстарое\n\n## Б\nвторое\n";
    let (out, _) = replace_md_section(md, "А", "новое\n").unwrap();
    assert!(
        out.contains("новое\n\n## Б"),
        "пустая строка между секциями восстановлена"
    );
}

// ——— твин-гард (перенесённые; рецепт в сообщении — на поверхность MCP) ———

#[test]
fn h3_twin_refuses_with_level_and_byte_line() {
    let md = "## Родитель\nввод\n### Подсекция\nдетали подсекции\n";
    let err = replace_md_section(md, "Подсекция", "новое\n").unwrap_err();
    assert!(err.contains("h3"), "уровень тёзки в ошибке: {err}");
    assert!(
        err.contains("### Подсекция"),
        "байт-точная строка заголовка: {err}"
    );
    assert!(
        err.contains("pinned `### Подсекция`"),
        "рецепт пина в ошибке: {err}"
    );
    assert!(
        err.contains("task_body"),
        "рецепт полного body в ошибке: {err}"
    );
}

#[test]
fn h1_twin_refuses_replacement() {
    let md = "# Подсекция\nтело тайтла\n";
    let err = replace_md_section(md, "Подсекция", "новое\n").unwrap_err();
    assert!(
        err.contains("h1"),
        "h1-тёзка гардится тем же правилом: {err}"
    );
}

#[test]
fn twin_detection_is_normalized_double_space_and_block_id() {
    // регулярный прод-экспорт несёт двойные пробелы и ^block-id: детект
    // тёзки по сырой строке промолчал бы ровно на этом случае
    let md = "## Родитель\nввод\n###  Подсекция ^20261105230000-ab12cd\nдетали\n";
    let err = replace_md_section(md, "Подсекция", "новое\n").unwrap_err();
    assert!(
        err.contains("###  Подсекция ^20261105230000-ab12cd"),
        "байт-точная строка с пробелами и ^id: {err}"
    );
}

#[test]
fn twin_inside_fence_appends_instead_of_guard() {
    let md = "## Родитель\n```md\n### Подсекция\n```\nхвост\n";
    let (out, edit) = replace_md_section(md, "Подсекция", "новое\n").unwrap();
    assert!(
        out.contains("## Подсекция\nновое"),
        "тёзка в фенсе — не заголовок, гард молчит: легитимный append"
    );
    assert_eq!(edit, SectionEdit::Appended);
}

#[test]
fn h2_match_wins_over_h3_twin_without_guard() {
    let md = "## Подсекция\nстарое\n\n## Другое\nввод\n### Подсекция\nдетали\n";
    let (out, edit) = replace_md_section(md, "Подсекция", "новое\n").unwrap();
    assert!(
        out.contains("## Подсекция\nновое"),
        "h2-матч заменён на месте"
    );
    assert!(
        out.contains("### Подсекция\nдетали"),
        "###-тёзка вне секции не тронута"
    );
    assert_eq!(
        edit,
        SectionEdit::Replaced {
            duplicates_removed: 0
        },
        "гард не стреляет при живом h2"
    );
}

#[test]
fn first_twin_in_document_order_named_in_error() {
    let md = "### Подсекция\nверхняя\n\n# Подсекция\nнижняя\n";
    let err = replace_md_section(md, "Подсекция", "новое\n").unwrap_err();
    assert!(
        err.contains("h3"),
        "первая тёзка по порядку — детерминизм: {err}"
    );
    assert!(err.contains("### Подсекция"), "строка первой тёзки: {err}");
    assert!(!err.contains("h1"), "вторая тёзка не упоминается: {err}");
}

// ——— переписанные под пин (фикс-раунд aa28614: срез префикса → пин) ———

#[test]
fn prefixed_h3_query_replaces_live_h3() {
    // легаси-исход был отказом (срез префикса до bare + твин-гард); под
    // пином «### X» заменяет живую h3-секцию — первый потребитель кейса
    // #1046 (H3-нутро «Отчёта инженера»)
    let md = "## Финальный отчёт\nввод\n### Замечания и неочевидные решения\nстарые детали\n\n## Комменты\n- строка\n";
    let (out, edit) = replace_md_section(
        md,
        "### Замечания и неочевидные решения",
        "новые формулировки\n",
    )
    .unwrap();
    assert!(out.contains("### Замечания и неочевидные решения\nновые формулировки"));
    assert!(!out.contains("старые детали"), "старый контент h3 заменён");
    assert!(
        out.contains("## Финальный отчёт\nввод"),
        "родительская h2 не тронута"
    );
    assert!(
        out.contains("## Комменты\n- строка"),
        "соседняя h2 не тронута"
    );
    assert_eq!(
        edit,
        SectionEdit::Replaced {
            duplicates_removed: 0
        }
    );
}

#[test]
fn prefixed_h2_query_matches_live_section() {
    // исход тот же, что в легаси («## X» матчит «X»), точка нормализации
    // другая: префикс пинит h2, а не срезается
    let md = "## X\nстарое\n\n## Б\nвторое\n";
    let (out, edit) = replace_md_section(md, "## X", "новое\n").unwrap();
    assert!(
        out.contains("## X\nновое"),
        "запрос «## X» матчит живую секцию «X»"
    );
    assert!(out.contains("## Б\nвторое"));
    assert_eq!(out.matches("## X").count(), 1, "без дубля секции");
    assert_eq!(
        edit,
        SectionEdit::Replaced {
            duplicates_removed: 0
        }
    );
}

#[test]
fn append_writes_clean_heading_bare_and_prefixed_h2() {
    for query in ["Новая", "## Новая"] {
        let md = "## А\nтело\n";
        let (out, edit) = replace_md_section(md, query, "контент").unwrap();
        assert!(
            out.contains("## Новая\nконтент"),
            "append чистым заголовком: {query}"
        );
        assert!(
            !out.contains("## ##"),
            "двойного префикса в теле нет: {out}"
        );
        assert_eq!(edit, SectionEdit::Appended, "{query}");
    }
}

#[test]
fn clear_request_for_absent_section_refuses() {
    // очистка отсутствующей секции создала бы пустую «## X» — исход
    // неожидаем (находка дельта-ревью #1152): громкий отказ, записи нет
    let md = "## А\nтело\n";
    for query in ["Новая", "## Новая"] {
        let err = replace_md_section(md, query, "").unwrap_err();
        assert!(err.contains("nothing to clear"), "{query}: {err}");
        assert!(
            err.contains("not found"),
            "причина — отсутствие секции: {err}"
        );
    }
}

#[test]
fn prefixed_h3_query_absent_section_refuses_no_append() {
    // легаси-факт 06.11: prefixed-h3 при отсутствии секции писал мусор
    // «## ### …» — под пином громкий отказ (append только h2)
    let md = "## А\nтело\n";
    let err = replace_md_section(md, "### Новая", "контент").unwrap_err();
    assert!(
        err.contains("at h3 not found"),
        "уровень пина в ошибке: {err}"
    );
    assert!(
        err.contains("append is for h2 only"),
        "причина отказа: {err}"
    );
}

// ——— новые пин-кейсы ———

#[test]
fn pinned_h3_replaces_inside_parent_neighbors_intact() {
    let md = "# Таск\n\n## Родитель\nввод\n### Подсекция\nстарые детали\n\n## Другое\nвторое\n";
    let (out, edit) = replace_md_section(md, "### Подсекция", "новые детали\n").unwrap();
    assert!(
        out.contains("### Подсекция\nновые детали"),
        "h3 заменена на месте"
    );
    assert!(!out.contains("старые детали"), "старый контент h3 удалён");
    assert!(out.contains("# Таск\n"), "h1-сосед не тронут");
    assert!(
        out.contains("## Родитель\nввод"),
        "родительская h2 не тронута"
    );
    assert!(out.contains("## Другое\nвторое"), "соседняя h2 не тронута");
    assert_eq!(
        edit,
        SectionEdit::Replaced {
            duplicates_removed: 0
        }
    );
}

#[test]
fn h4_inside_h3_is_not_boundary() {
    let md = "## Родитель\n### Секция\nверх\n#### Глубоко\nхвост глубоко\n\n### Другая\nниз\n";
    let (out, _) = replace_md_section(md, "### Секция", "новое\n").unwrap();
    assert!(
        !out.contains("хвост глубоко"),
        "h4 внутри h3-секции — её контент"
    );
    assert!(
        out.contains("### Другая\nниз"),
        "следующая h3 — граница, жива"
    );
}

#[test]
fn pinned_h3_absent_with_h2_twin_refuses_with_pin_hint() {
    let md = "## Подсекция\nввод\n";
    let err = replace_md_section(md, "### Подсекция", "новое\n").unwrap_err();
    assert!(
        err.contains("namesake exists at h2"),
        "уровень тёзки: {err}"
    );
    assert!(err.contains("«## Подсекция»"), "байт-точная строка: {err}");
    assert!(
        err.contains("pinned `## Подсекция`"),
        "подсказка пина: {err}"
    );
}

#[test]
fn hash_prefixed_h2_is_equivalent_to_bare() {
    let md = "## А\nстарое\n";
    let (bare, bare_edit) = replace_md_section(md, "А", "новое\n").unwrap();
    let (pinned, pinned_edit) = replace_md_section(md, "## А", "новое\n").unwrap();
    assert_eq!(bare, pinned, "«## X» ≡ bare на замене");
    assert_eq!(bare_edit, pinned_edit);
    let (bare_app, _) = replace_md_section("## А\nтело\n", "Новая", "к").unwrap();
    let (pin_app, _) = replace_md_section("## А\nтело\n", "## Новая", "к").unwrap();
    assert_eq!(bare_app, pin_app, "«## X» ≡ bare на append");
}

#[test]
fn h1_pin_replaces_h1_section_with_document_content() {
    // граница h1-секции — следующий заголовок уровня ≤ 1: h2 внутри
    // h1-секции — её контент (семантика границы уровня пина)
    let md = "# Титул\nстарый тайтл\n\n## Постановка\nтело\n";
    let (out, edit) = replace_md_section(md, "# Титул", "новый тайтл\n").unwrap();
    assert_eq!(
        out, "# Титул\nновый тайтл\n",
        "h1-секция длится до следующего h1"
    );
    assert_eq!(
        edit,
        SectionEdit::Replaced {
            duplicates_removed: 0
        }
    );
}

#[test]
fn h1_pin_absent_refuses() {
    let md = "## Постановка\nтело\n";
    let err = replace_md_section(md, "# Титул", "новое\n").unwrap_err();
    assert!(
        err.contains("at h1 not found"),
        "уровень пина в ошибке: {err}"
    );
    assert!(
        err.contains("append is for h2 only"),
        "причина отказа: {err}"
    );
}

#[test]
fn bare_query_with_live_h3_twin_and_no_h2_guards() {
    // не тихая замена h3: bare пинит h2, тёзка на h3 — громкий отказ
    let md = "### Подсекция\nдетали\n";
    let err = replace_md_section(md, "Подсекция", "новое\n").unwrap_err();
    assert!(
        err.contains("namesake exists at h3"),
        "гард, не замена: {err}"
    );
    assert!(
        err.contains("pinned `### Подсекция`"),
        "подсказка пина: {err}"
    );
}

// ——— body_hash (FNV-1a 64) ———

#[test]
fn body_hash_matches_fnv1a_known_vectors() {
    assert_eq!(body_hash(""), "cbf29ce484222325", "offset basis");
    assert_eq!(body_hash("a"), "af63dc4c8601ec8c");
    assert_eq!(body_hash("foobar"), "85944171f73967e8");
}

#[test]
fn body_hash_is_stable_hex16() {
    let h = body_hash("## Постановка\n\nТекст.\n");
    assert_eq!(h.len(), 16, "64-битный hex без ведущего нуля-обрезки");
    assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(
        h,
        body_hash("## Постановка\n\nТекст.\n"),
        "стабилен на равных телах"
    );
    assert_ne!(
        body_hash("тело А"),
        body_hash("тело Б"),
        "разные тела — разные хэши"
    );
}
