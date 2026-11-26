//! Пины носителя промптов: методичка живёт в `agents/*.md`, сервер шлёт
//! данные, маркер и указатель. Дефы читаются по пути внутри репо, а не
//! через HOME-симлинк исполнителя (урок «тесты против машины»).

use crate::pipeline::actions::{spawn_prompt, STAGE_PROMPTS};
use crate::pipeline::analyst::analyst_prompt;
use crate::pipeline::analyst_review::analyst_review_prompt;
use crate::pipeline::tick::BoardPlan;
use uwuwu_domain::{Status, Task};

fn agent_doc(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../agents")
        .join(format!("{name}.md"));
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("agent def {} unreadable: {e}", path.display()))
}

fn board() -> BoardPlan {
    BoardPlan {
        board: "B".into(),
        project: "p".into(),
        repo: "a/c".into(),
        checkout: "/tmp/checkout".into(),
        env_root: "/tmp/env".into(),
    }
}

#[test]
fn agent_defs_carry_the_methodology_the_server_no_longer_sends() {
    // Arrange
    let analyst = agent_doc("analyst");
    let reviewer = agent_doc("analyst-reviewer");
    let dreamer = agent_doc("dreamer");
    let tester = agent_doc("tester");
    let developer = agent_doc("developer");

    // Assert
    assert!(analyst.contains("sourcing") && analyst.contains("TASK-TEMPLATE"));
    assert!(reviewer.contains("Sourcing") && reviewer.contains("Verdicts"));
    assert!(dreamer.contains("Experience"));
    assert!(tester.contains("Delta mode"));
    assert!(developer.contains("PLANNING") && developer.contains("IMPLEMENTATION"));
}

#[test]
fn developer_def_maps_both_stage_markers_to_phases() {
    let developer = agent_doc("developer");
    assert!(developer.contains("stage=developer/design"));
    assert!(developer.contains("stage=developer/dev"));
}

#[test]
fn stage_markers_are_unambiguous() {
    let markers: Vec<&str> = STAGE_PROMPTS.iter().map(|(_, m)| *m).collect();
    for (i, left) in markers.iter().enumerate() {
        for right in markers.iter().skip(i + 1) {
            assert!(!left.contains(right) && !right.contains(left));
        }
        let status_names = [
            "Draft",
            "Analys",
            "Review Analys",
            "Todo",
            "Design",
            "Plan review",
            "In Progress",
            "Result review",
            "Testing",
            "Testing review",
            "Review",
            "Done",
            "Needs attention",
            "Wait",
            "Deleted",
        ];
        assert!(!status_names.contains(left));
    }
    assert_eq!(markers.len(), 6);
}

#[test]
fn spawn_prompt_carries_data_marker_pointer_and_contract() {
    let task = Task::new(7, "t".into(), Status::Design);
    let prompt = spawn_prompt(&board(), &task, uwuwu_domain::Slot::Design);

    assert!(prompt.contains("#7") && prompt.contains("B") && prompt.contains("p"));
    assert!(prompt.contains("stage=developer/design"));
    assert!(prompt.contains("Methodology: agents/developer.md"));
    assert!(prompt.contains("SLEEPS after the reply"));
    assert!(prompt.contains("uwuwu_person_show") && prompt.contains("uwuwu_docs_read"));
}

#[test]
fn analyst_waves_point_to_their_defs_and_keep_owner_answer() {
    let task = Task::new(9, "t".into(), Status::Analys);
    let wave = analyst_prompt("p", &[&task]);
    let wave_review = analyst_review_prompt("p", &[&task]);

    assert!(wave.contains("Methodology: agents/analyst.md"));
    assert!(wave_review.contains("Methodology: agents/analyst-reviewer.md"));
    assert!(wave.contains("Analytics wave") && wave_review.contains("Analytics review"));
    for prompt in [&wave, &wave_review] {
        assert!(prompt.contains("owner_answer") && prompt.contains("verbatim"));
    }
}
