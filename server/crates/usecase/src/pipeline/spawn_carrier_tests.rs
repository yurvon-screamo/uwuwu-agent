//! Пины тонких промптов спавна (задача #1647, Testing).
//!
//! Что ловим: дрейф связки «маркер слота → указатель на деф» и потерю
//! контрактов UC-11/UC-05 в тонких промптах. Каждый слот обязан нести СВОЙ
//! маркер из `STAGE_PROMPTS` и СВОЙ указатель `Methodology: agents/<name>.md`;
//! имя указателя обязано резолвиться в существующий в репо деф (иначе —
//! указатель в пустоту и тихий фолбэк исполнителя). Потеря указателя =
//! сессия не найдёт методичку. Мок-стенд классифицирует спавн только по
//! маркерам `STAGE_PROMPTS` и маркерам волн (`mock_opencode`), текст
//! `Methodology:` он не читает — чужой маркер ломает стенд, чужой
//! указатель уводит сессию мимо стенда.

use crate::error::UsecaseResult;
use crate::naming;
use crate::pipeline::actions::{spawn_prompt, STAGE_PROMPTS};
use crate::pipeline::cup_pins_tests::DreamerClock;
use crate::pipeline::degraded_list_support::dreamer_target;
use crate::pipeline::dreamer::DreamerUseCase;
use crate::pipeline::tick::BoardPlan;
use crate::pipeline::tick_tests::empty_tick_state;
use crate::ports::{
    AgentSessionGateway, AssistantReply, Clock, Liveness, SessionInfo, SessionSpec, TickStateStore,
};
use crate::router::SessionRouter;
use crate::spawn_models::SpawnModels;
use async_trait::async_trait;
use std::sync::{Arc, Mutex};
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

/// Деф обязан существовать в репо (путь — от манифеста крейта, как прод
/// его не читает, а тесты дефов; не HOME-симлинк исполнителя).
fn agent_def_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../agents")
        .join(format!("{name}.md"))
}

#[test]
fn spawn_every_slot_carries_its_own_marker_and_existing_def_pointer() {
    // Arrange
    let board = board();
    assert_eq!(
        all_slots().len(),
        STAGE_PROMPTS.len(),
        "новый слот обязан попасть и в список, и в STAGE_PROMPTS"
    );

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
        let def_name = naming::agent_name(agent);
        let pointer = format!("Methodology: agents/{def_name}.md");

        assert!(
            prompt.contains(marker),
            "слот {slot:?}: потерян свой маркер «{marker}»"
        );
        assert!(
            prompt.contains(&pointer),
            "слот {slot:?}: потерян указатель «{pointer}»"
        );
        assert!(
            agent_def_path(def_name).is_file(),
            "слот {slot:?}: указатель «{pointer}» не резолвится — дефа нет в репо"
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
fn spawn_every_slot_keeps_turn_contract_and_bootstrap_hints() {
    // UC-11 (контракт хода) и UC-05 (персона/доки проекта) остаются в Rust
    // на КАЖДОМ сайте спавна, а не только в Design (деф их не дублирует).
    // Ассерты — фрагментами клауз, а не одиночными токенами: токен
    // «Needs attention» встречается и в абзаце про ответ владельца, токен
    // «uwuwu_task_edit» — там же, и удаление целой опции их не краснило.
    let board = board();
    for (i, slot) in all_slots().into_iter().enumerate() {
        let task = Task::new(200 + i as u64, "t".into(), Status::Design);
        let prompt = spawn_prompt(&board, &task, slot);

        for clause in [
            "SLEEPS after the reply",
            "a status transition (uwuwu_task_edit",
            "+ status\n«Needs attention»",
            "owner_answer",
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

/// Гейтвей с памятью текстов: записывает каждый доставленный промпт.
/// Только in-memory, без сети (образец — `RecordingGw`, ему не хватает
/// именно текста).
struct PromptRecordingGw {
    sessions: Vec<SessionInfo>,
    created: Arc<Mutex<Vec<String>>>,
    prompted: Arc<Mutex<Vec<(String, String)>>>,
}

impl PromptRecordingGw {
    fn new(sessions: Vec<SessionInfo>) -> Arc<Self> {
        Arc::new(Self {
            sessions,
            created: Arc::new(Mutex::new(Vec::new())),
            prompted: Arc::new(Mutex::new(Vec::new())),
        })
    }

    fn prompt_texts(&self) -> Vec<(String, String)> {
        self.prompted.lock().expect("prompted lock").clone()
    }
}

#[async_trait]
impl AgentSessionGateway for PromptRecordingGw {
    async fn create_session(&self, _spec: &SessionSpec) -> UsecaseResult<String> {
        self.created
            .lock()
            .expect("created lock")
            .push("new".into());
        Ok("ses_new".into())
    }
    async fn prompt(&self, session: &str, text: &str) -> UsecaseResult<()> {
        self.prompted
            .lock()
            .expect("prompted lock")
            .push((session.into(), text.into()));
        Ok(())
    }
    async fn run_command(&self, _s: &str, _n: &str, _a: &str, _b: u64) -> UsecaseResult<()> {
        Ok(())
    }
    async fn liveness(&self, _s: &str) -> Liveness {
        Liveness::Alive
    }
    async fn delete_session(&self, _s: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn last_activity_ms(&self, _s: &str) -> UsecaseResult<Option<u64>> {
        Ok(None)
    }
    async fn last_assistant_reply(&self, _s: &str) -> UsecaseResult<Option<AssistantReply>> {
        Ok(None)
    }
    async fn list_sessions(&self) -> UsecaseResult<Vec<SessionInfo>> {
        Ok(self.sessions.clone())
    }
    async fn export_session(&self, _s: &str) -> UsecaseResult<String> {
        Ok(String::new())
    }
}

fn dreamer_usecase(sessions: Arc<SessionRouter>, state: Arc<dyn TickStateStore>) -> DreamerUseCase {
    DreamerUseCase {
        sessions,
        state,
        clock: Arc::new(DreamerClock),
        models: Arc::new(SpawnModels::canonical()),
        checkout: "/tmp/checkout".into(),
        cup: 7,
    }
}

fn live_dreamer_session(id: &str) -> SessionInfo {
    SessionInfo {
        id: id.into(),
        title: String::new(),
        agent: String::new(),
        directory: String::new(),
        updated_ms: DreamerClock.now_ms(),
        outcome: "succeeded".into(),
        total_tokens: 1,
        busy: false,
    }
}

#[tokio::test]
async fn dreamer_spawn_prompt_carries_pointer_and_export_form() {
    // Arrange: линка нет — первый прогон идёт спавном нового носителя.
    let gw = PromptRecordingGw::new(vec![dreamer_target()]);
    let state = empty_tick_state();

    // Act
    let spawned = dreamer_usecase(Arc::new(SessionRouter::uniform(gw.clone())), state)
        .tick()
        .await
        .expect("дример-спавн не роняет тик");

    // Assert
    assert_eq!(spawned, 1, "первый прогон — спавн носителя");
    let texts = gw.prompt_texts();
    assert_eq!(texts.len(), 1, "ровно один промпт: {texts:?}");
    let (session, text) = &texts[0];
    assert_eq!(session, "ses_new", "промпт — новому носителю");
    for clause in [
        "Methodology: agents/dreamer.md",
        "uwuwu_session_export {session: \"ses_target\"}",
    ] {
        assert!(
            text.contains(clause),
            "спавн-промпт дримера потерял «{clause}»: {text}"
        );
    }
}

#[tokio::test]
async fn dreamer_wakeup_prompt_carries_pointer_and_export_form() {
    // Arrange: линк жив — повторный прогон идёт wakeup существующего носителя.
    let gw = PromptRecordingGw::new(vec![dreamer_target(), live_dreamer_session("ses_dreamer")]);
    let state = empty_tick_state();
    state.link_dreamer_session("ses_target", "ses_dreamer");

    // Act
    let acted = dreamer_usecase(Arc::new(SessionRouter::uniform(gw.clone())), state)
        .tick()
        .await
        .expect("дример-wakeup не роняет тик");

    // Assert
    assert_eq!(acted, 1, "повторный прогон — wakeup носителя");
    assert!(
        gw.created.lock().expect("created lock").is_empty(),
        "wakeup не спавнит новый носитель"
    );
    let texts = gw.prompt_texts();
    assert_eq!(texts.len(), 1, "ровно один промпт: {texts:?}");
    let (session, text) = &texts[0];
    assert_eq!(session, "ses_dreamer", "промпт — живому носителю");
    for clause in [
        "Methodology: agents/dreamer.md",
        "uwuwu_session_export {session: \"ses_target\"}",
        "only the delta",
    ] {
        assert!(
            text.contains(clause),
            "wakeup-промпт дримера потерял «{clause}»: {text}"
        );
    }
}
