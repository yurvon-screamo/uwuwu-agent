//! Тестерские пины носителя (задача #1647, Testing).
//!
//! Что ловим: дрейф связки «маркер слота → указатель на деф» и потерю
//! контрактов UC-11/UC-05 в тонких промптах. Каждый слот обязан нести СВОЙ
//! маркер из `STAGE_PROMPTS` и СВОЙ указатель `Methodology: agents/<name>.md`
//! (имя — то же отображение `Agent::of_slot` + `naming::agent_name`,
//! что использует прод). Потеря указателя = сессия не найдёт методичку;
//! чужой маркер/указатель = мок-стенд классифицирует слот неверно.

use crate::naming;
use crate::pipeline::actions::{spawn_prompt, STAGE_PROMPTS};
use crate::pipeline::analyst::analyst_prompt;
use crate::pipeline::analyst_review::analyst_review_prompt;
use crate::pipeline::tick::BoardPlan;
use uwuwu_domain::{Agent, Slot, Status, Task};

fn board() -> BoardPlan {
    BoardPlan {
        board: "BOARD-TESTER".into(),
        project: "proj-tester".into(),
        repo: "a/c".into(),
        checkout: "/tmp/checkout".into(),
        env_root: "/tmp/env".into(),
    }
}

fn all_slots() -> [Slot; 6] {
    [
        Slot::Design,
        Slot::Dev,
        Slot::PlanReview,
        Slot::ResultReview,
        Slot::Test,
        Slot::TestReview,
    ]
}

#[test]
fn tester_every_slot_carries_its_own_marker_and_def_pointer() {
    // Arrange
    let board = board();

    // Act + Assert — по каждому слоту отдельно, чтобы падение указало на слот.
    for (i, slot) in all_slots().into_iter().enumerate() {
        let task = Task::new(100 + i as u64, "t".into(), Status::Design);
        let prompt = spawn_prompt(&board, &task, slot);
        let marker = STAGE_PROMPTS
            .iter()
            .find(|(s, _)| *s == slot)
            .map(|(_, m)| *m)
            .expect("STAGE_PROMPTS обязан покрывать все слоты");
        let agent = Agent::of_slot(slot).expect("у каждого слота есть агент");
        let pointer = format!("Methodology: agents/{}.md", naming::agent_name(agent));

        assert!(
            prompt.contains(marker),
            "слот {slot:?}: потерян свой маркер «{marker}»"
        );
        assert!(
            prompt.contains(&pointer),
            "слот {slot:?}: потерян указатель «{pointer}»"
        );
        // Чужой маркер в промпте — неверная классификация моком.
        for (other_slot, other_marker) in STAGE_PROMPTS.iter() {
            if *other_slot != slot {
                assert!(
                    !prompt.contains(other_marker),
                    "слот {slot:?}: чужой маркер «{other_marker}» слота {other_slot:?}"
                );
            }
        }
    }
}

#[test]
fn tester_every_slot_keeps_turn_contract_and_bootstrap_hints() {
    // UC-11 (контракт хода) и UC-05 (персона/доки проекта) остаются в Rust
    // на КАЖДОМ сайте спавна, а не только в Design (деф их не дублирует).
    let board = board();
    for (i, slot) in all_slots().into_iter().enumerate() {
        let task = Task::new(200 + i as u64, "t".into(), Status::Design);
        let prompt = spawn_prompt(&board, &task, slot);

        for clause in [
            "SLEEPS after the reply",
            "owner_answer",
            "Needs attention",
            "uwuwu_task_edit",
        ] {
            assert!(
                prompt.contains(clause),
                "слот {slot:?}: контракт хода потерял «{clause}»"
            );
        }
        for hint in ["uwuwu_person_show", "uwuwu_docs_read"] {
            assert!(
                prompt.contains(hint),
                "слот {slot:?}: bootstrap-подсказка UC-05 потеряла «{hint}»"
            );
        }
    }
}

#[test]
fn tester_waves_keep_markers_pointers_and_owner_answer() {
    // Маркер волны + указатель на деф + клауза owner_answer — контракт
    // UC-11 на сайтах волн (дополняет пин дефов: там методика, тут — сайт).
    let task = Task::new(300, "t".into(), Status::Analys);
    let wave = analyst_prompt("p", &[&task]);
    let review = analyst_review_prompt("p", &[&task]);

    assert!(
        wave.contains("Analytics wave"),
        "волна аналитика без маркера: {wave}"
    );
    assert!(
        review.contains("Analytics review"),
        "волна ревьювера без маркера: {review}"
    );
    assert!(
        wave.contains("Methodology: agents/analyst.md"),
        "волна аналитика без указателя на деф"
    );
    assert!(
        review.contains("Methodology: agents/analyst-reviewer.md"),
        "волна ревьювера без указателя на деф"
    );
    for prompt in [&wave, &review] {
        assert!(
            prompt.contains("owner_answer") && prompt.contains("verbatim"),
            "волна без клаузы owner_answer/verbatim: {prompt}"
        );
    }
}
