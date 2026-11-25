//! Юнит-пины slug-гарда (#1613): ветки отказа, легаси-стем, деградация
//! (пустой live — root жив, parent/suffix пропущены), граница сегментов.

use crate::slug_guard::{check, suffix_candidates, SlugVerdict};

fn live(slugs: &[&str]) -> Vec<String> {
    slugs.iter().map(|s| s.to_string()).collect()
}

// --- root-prefix ---

#[test]
fn unknown_root_refused_with_suffix_candidates() {
    // класс «совпавший тайтл» #1219: кандидат называет живую статью
    let v = check("cli/статья", &live(&["Experience/cli/статья"]));
    assert_eq!(
        v,
        SlugVerdict::RootPrefix(vec!["Experience/cli/статья".to_string()])
    );
}

#[test]
fn unknown_root_unique_article_has_no_candidates() {
    // подкласс «уникальная статья в чужом корне» (#1219/#1575): хвост не совпал
    let v = check("cli/уникальная", &live(&["Experience/cli/другая"]));
    assert_eq!(v, SlugVerdict::RootPrefix(Vec::new()));
}

#[test]
fn single_segment_target_is_file_in_root() {
    // L-5 р3: слаг «Experience» создал бы docs/Experience.md рядом с папкой
    assert_eq!(
        check("Experience", &live(&["Experience/platform/x"])),
        SlugVerdict::RootPrefix(Vec::new())
    );
    // одно-сегментный таргет вне канона — с кандидатом при живом хвостовом доке
    assert_eq!(
        check("platform", &live(&["Experience/platform"])),
        SlugVerdict::RootPrefix(vec!["Experience/platform".to_string()])
    );
}

#[test]
fn dash_neighbor_does_not_collide() {
    // граница сегментов: «cli-статья» не является путём «cli/статья»
    let v = check("cli/статья", &live(&["Experience/cli-статья"]));
    assert_eq!(v, SlugVerdict::RootPrefix(Vec::new()));
}

// --- parent-collision ---

#[test]
fn plain_file_parent_refused() {
    let v = check(
        "Projects/uwuwu/Design Spec/док",
        &live(&["Projects/uwuwu/README", "Projects/uwuwu/Design Spec"]),
    );
    assert_eq!(
        v,
        SlugVerdict::ParentCollision("Projects/uwuwu/Design Spec".to_string())
    );
}

#[test]
fn legacy_stem_file_plus_dir_allows_write() {
    // H-1 р1: 20 живых пар «ADR.md + ADR/» — файл и папка одновременно;
    // новый док под легаси-стемом пишется, отказ запрещён
    let v = check(
        "Projects/uwuwu/ADR/ADR-114: Новый",
        &live(&["Projects/uwuwu/ADR", "Projects/uwuwu/ADR/ADR-001"]),
    );
    assert_eq!(v, SlugVerdict::Clear);
}

#[test]
fn canonical_multisegment_new_doc_clear() {
    // родитель — только неявная дир-узел (папка), файла-родителя нет
    let v = check(
        "Experience/platform/новая-статья",
        &live(&["Experience/platform/старая"]),
    );
    assert_eq!(v, SlugVerdict::Clear);
}

// --- suffix-collision ---

#[test]
fn suffix_collision_under_canonical_root() {
    // вырожденный класс вложенного корнеимени (ноль на живом корпусе)
    let v = check("Projects/x", &live(&["Experience/Projects/x"]));
    assert_eq!(
        v,
        SlugVerdict::SuffixCollision(vec!["Experience/Projects/x".to_string()])
    );
}

#[test]
fn exact_match_is_not_a_collision_branch() {
    // противоречие устаревшего дерева с авторитетным чтением решает
    // проводка (read-first); предикат точное совпадение не классифицирует
    let v = check("Experience/platform/x", &live(&["Experience/platform/x"]));
    assert_eq!(v, SlugVerdict::Clear);
}

// --- деградация: пустой live ---

#[test]
fn degraded_empty_live_keeps_root_check() {
    // 502-шторм: подкласс #1575 закрыт и без дерева
    assert_eq!(check("cli/x", &[]), SlugVerdict::RootPrefix(Vec::new()));
    assert_eq!(check("Experience", &[]), SlugVerdict::RootPrefix(Vec::new()));
}

#[test]
fn degraded_empty_live_skips_parent_and_suffix() {
    // деградационное окно: parent/suffix-проверки дерева не видят
    assert_eq!(check("Experience/platform/x", &[]), SlugVerdict::Clear);
}

// --- suffix_candidates ---

#[test]
fn suffix_candidates_needs_segment_boundary_and_strictly_longer() {
    let c = suffix_candidates(
        "cli/статья",
        &live(&[
            "Experience/cli/статья",
            "Experience/cli-статья",
            "cli/статья",
        ]),
    );
    assert_eq!(c, vec!["Experience/cli/статья".to_string()]);
}
