//! Пины носителя промптов: методичка живёт в `agents/*.md`, сервер шлёт
//! данные, маркер и указатель. Дефы читаются по пути внутри репо, а не
//! через HOME-симлинк исполнителя (урок «тесты против машины»).
//! Клаузы, перенесённые из Rust-промптов в дефы (uwuwu#1647), закреплены
//! здесь: удаление клаузы из дефа должно делать тест красным.

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

/// Док в корне репо (AGENTS.md читается оттуда, не из `agents/`).
fn repo_file(rel: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("repo doc {} unreadable: {e}", path.display()))
}

fn assert_clauses(doc: &str, agent: &str, clauses: &[&str]) {
    for clause in clauses {
        assert!(doc.contains(clause), "{agent}: клауза «{clause}» потеряна");
    }
}

const BOARD: &str = "BOARD-1647";
const PROJECT: &str = "proj-1647";

fn board() -> BoardPlan {
    BoardPlan {
        board: BOARD.into(),
        project: PROJECT.into(),
        repo: "a/c".into(),
        checkout: "/tmp/checkout".into(),
        env_root: "/tmp/env".into(),
    }
}

#[test]
fn analyst_def_pins_clauses_migrated_from_rust_prompts() {
    let analyst = agent_doc("analyst");
    assert_clauses(
        &analyst,
        "analyst",
        &[
            "`task_get` each — the request body in full",
            "A requirement enters the statement **only from a source**",
            "Invented content is marked",
            "The elaboration artifact — **the reworked task body**",
            "per the TASK-TEMPLATE skeleton",
            "Proportionality of frames by type",
            "US/UC/NFT: local ID · frame · source",
            "a reproduction frame (you do not diagnose causes, no records)",
            "«what counts as the answer»",
            "Verdicts: elaborated",
            "into **Review Analys**",
            "duplicate/garbage →",
            "a real blocker →",
            "a fork outside FT/NFT →",
            "You do not implement the task",
            "do not write into the project's Design Spec",
            "You do not create tasks",
        ],
    );
}

#[test]
fn analyst_reviewer_def_pins_clauses_migrated_from_rust_prompts() {
    let reviewer = agent_doc("analyst-reviewer");
    assert_clauses(
        &reviewer,
        "analyst-reviewer",
        &[
            "**Sourcing (the main optic, ADR-102):**",
            "Invented = request_changes.",
            "**Form/dedup:**",
            "**Proportionality:**",
            "**Readiness:**",
            "`task_get` — the reworked body; verify against the request's sources",
            "Verdicts: approve",
            "into **Todo**",
            "request_changes →",
            "**Needs attention**",
            "The report — one comment per row",
            "no separate wave summary posts",
        ],
    );
}

#[test]
fn dreamer_def_pins_clauses_migrated_from_rust_prompts() {
    let dreamer = agent_doc("dreamer");
    assert_clauses(
        &dreamer,
        "dreamer",
        &[
            "Per-session run: the dump in full — the command from the prompt",
            "### 4. Project documentation hygiene",
            "Read the project docs **live**",
            "### 5. Experience (always)",
            "- ADR: if the session shows an accepted architectural decision",
            "Extract transferable knowledge",
            "**mandatory dedup**",
            "A close article exists —",
            "none — create one",
            "Nothing to take — a normal",
        ],
    );
}

#[test]
fn developer_def_maps_both_stage_markers_to_phases() {
    // Arrange
    let developer = agent_doc("developer");

    // Act — позиции клауз и обоих заголовков фаз.
    let planning = developer.find("# PHASE: PLANNING").unwrap();
    let implementation = developer.find("# PHASE: IMPLEMENTATION").unwrap();
    let design = developer.find("stage=developer/design").unwrap();
    let dev = developer.find("stage=developer/dev").unwrap();

    // Assert — перестановка маркеров между фазами ломает пин.
    assert!(planning < design && design < implementation);
    assert!(implementation < dev);
}

#[test]
fn tester_def_pins_reentry_delta_mode() {
    let tester = agent_doc("tester");
    assert_clauses(
        &tester,
        "tester",
        &["**Delta mode (a re-entry after the bug loop)**"],
    );
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

    assert!(prompt.contains("#7") && prompt.contains(BOARD) && prompt.contains(PROJECT));
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

/// #1734: каждый гейт обязан спрашивать про архитектуру UI — клауза живёт
/// в методичке агента, которую спавн-карьера отдаёт по указателю выше.
/// Удаление или переформулировка любой клаузы должна сделать пин красным.
#[test]
fn ui_architecture_gate_clauses_are_present_in_agent_defs() {
    let cases: &[(&str, &[&str])] = &[
        (
            "developer",
            &[
                "A UI slice must name the layout mechanism (the built-in primitives of the stack) and the alternative. Manual geometry needs a reason and a ceiling: the conditions that trigger a switch to the primitives.",
                "When you fix a named root cause, grep the whole repository for that mechanism. Put unhandled occurrences into the report with a reason.",
                "A UI change that alters layout needs a matrix of width in points × DPR. For a webview, check the fresh build, not only the headless rig.",
            ],
        ),
        (
            "plan-reviewer",
            &[
                "Scope growth from a point fix to a rewrite without the owner's verbatim sanction is High. A `fact_track` row with a structural rewrite goes to the owner as a question before approval.",
                "A UI plan with manual geometry and no comparison against the stack's built-in primitives is High (the class “violations of the project's architecture”).",
            ],
        ),
        (
            "code-quality-reviewer",
            &[
                "For a fix by mechanism, check all occurrences of the mechanism. Deliver either a class fix or an explicit exception list with reasons.",
                "Growing file-complexity in a touched file between tasks is Common at minimum. A note in the file header does not close it.",
            ],
        ),
        (
            "tester",
            &["Zoom is a required state next to empty, loading, and error states. Run the matrix from the plan, not a single size."],
        ),
        (
            "dreamer",
            &["For a task with a UI delta, find decisions nobody made explicitly (custom mechanisms, manual geometry). If there is no ADR or record, flag it to the owner."],
        ),
    ];

    for (agent, clauses) in cases {
        let doc = agent_doc(agent);
        assert_clauses(&doc, agent, clauses);
    }
}

/// #1734, срез 2: точка остановки `fact_track` при выросшем скопе — в general-каноне.
#[test]
fn fact_track_scope_growth_clause_is_present_in_agents_md() {
    let canon = repo_file("AGENTS.md");
    assert_clauses(
        &canon,
        "AGENTS.md",
        &["Скоп `fact_track`-строки вырос до перекладки → агент останавливается на Needs attention с вопросом о переводе в Design."],
    );
}
