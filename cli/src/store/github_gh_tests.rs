//! Юнит-контракт gh_pages — полной пагинации gh-листингов (ADR-020,
//! баг 1086): терминация на короткой странице, дедуп дрейф-дублей,
//! id-less элементы, инвариант O(1) для ≤100 элементов, громкий кап.

use crate::store::github_gh::{gh_pages, GH_PAGE_SIZE};
use anyhow::Result;
use serde_json::{json, Value};

fn page(ids: &[u64]) -> Vec<Value> {
    ids.iter().map(|id| json!({ "id": id })).collect()
}

/// Мультистраница: 100+100+37 — все элементы собраны, четвёртый вызов
/// не делается (короткая страница терминации).
#[tokio::test]
async fn собирает_страницы_до_короткой_и_не_ходит_дальше() {
    // Arrange: фитчер логирует вызовы, страницы 100/100/37
    let mut calls = Vec::new();
    let pages = [
        page(&(0..100).collect::<Vec<_>>()),
        page(&(100..200).collect::<Vec<_>>()),
        page(&(200..237).collect::<Vec<_>>()),
    ];
    let fetch = async |page: usize| -> Result<Vec<Value>> {
        calls.push(page);
        Ok(pages[page - 1].clone())
    };

    // Act
    let all = gh_pages(fetch).await.expect("выборка ок");

    // Assert
    assert_eq!(all.len(), 237, "весь хвост за первыми страницами собран");
    assert_eq!(calls, vec![1, 2, 3], "короткая страница терминации");
}

/// ≤ pageSize — ровно один вызов: инвариант O(1) типичного случая
/// (ADR-020 п.5), пагинация не должна удорожать малые листинги.
#[tokio::test]
async fn короткий_листинг_это_ровно_один_вызов() {
    // Arrange
    let mut calls = Vec::new();
    let fetch = async |n: usize| -> Result<Vec<Value>> {
        calls.push(n);
        Ok(page(&[1, 2, 3]))
    };

    // Act
    let all = gh_pages(fetch).await.expect("выборка ок");

    // Assert
    assert_eq!(all.len(), 3);
    assert_eq!(calls, vec![1], "второй вызов не нужен");
}

/// Дрейф-дубль между страницами (конкурентная мутация во время обхода)
/// дедупится по id — двойная строка не попадает в инвентарь.
#[tokio::test]
async fn дрейф_дубль_id_между_страницами_дедупится() {
    // Arrange: страница 1 полная; на странице 2 свежие дубли хвоста + новый
    let mut first = page(&(0..100).collect::<Vec<_>>());
    let mut second = page(&(99..101).collect::<Vec<_>>());
    first.append(&mut second);
    let pages = [first, Vec::new()];
    let fetch = async |page: usize| -> Result<Vec<Value>> { Ok(pages[page - 1].clone()) };

    // Act
    let all = gh_pages(fetch).await.expect("выборка ок");

    // Assert
    assert_eq!(all.len(), 101, "id 99 задвоен между страницами");
    let ids: Vec<u64> = all
        .iter()
        .filter_map(|v| v.get("id").and_then(Value::as_u64))
        .collect();
    assert_eq!(ids.iter().filter(|&&i| i == 99).count(), 1, "ровно один 99");
}

/// Элемент без id не дедупится (канон ADR-020: пустые id не дедупятся) —
/// проходит и не ломает обход.
#[tokio::test]
async fn элемент_без_id_проходит_без_дедупа() {
    // Arrange: страница с id-less элементами + короткая
    let items = vec![json!({"number": 1}), json!({"id": 2}), json!({"n": 3})];
    let pages = [items];
    let fetch = async |_page: usize| -> Result<Vec<Value>> { Ok(pages[0].clone()) };

    // Act
    let all = gh_pages(fetch).await.expect("выборка ок");

    // Assert
    assert_eq!(all.len(), 3, "все элементы, включая id-less");
}

/// Кап страниц: бесконечная полная страница → громкий отказ с диагностикой,
/// не тихая усечка (рецидив чинимого класса запрещён).
#[tokio::test]
async fn кап_страниц_это_громкий_отказ() {
    // Arrange: фитчер всегда отдаёт полную свежую страницу
    let fetch = async |n: usize| -> Result<Vec<Value>> {
        let base = (n as u64 - 1) * GH_PAGE_SIZE as u64;
        Ok(page(
            &(base..base + GH_PAGE_SIZE as u64).collect::<Vec<_>>(),
        ))
    };

    // Act
    let err = gh_pages(fetch).await.expect_err("кап — громкий отказ");

    // Assert
    assert!(err.to_string().contains("кап"), "диагностика капа: {err}");
    assert!(
        err.to_string().contains("неполна"),
        "честное слово об усечке: {err}"
    );
}
