use super::{gist_of, GoalsView};
use crate::error::UsecaseError;
use crate::error::ViewError;
use async_trait::async_trait;
use std::sync::Arc;

struct FakeGoals(Vec<super::GoalIssue>);

#[async_trait]
impl crate::goals::GoalsRepo for FakeGoals {
    async fn open_goals(&self, _project: &str) -> crate::UsecaseResult<Vec<super::GoalIssue>> {
        Ok(self.0.clone())
    }
}

fn view(goals: Vec<super::GoalIssue>) -> GoalsView {
    GoalsView::new(
        Arc::new(FakeGoals(goals)),
        vec!["uwuwu".into(), "origa".into()],
        "https://github.com/acme/hub/issues?q=is%3Aissue+is%3Aopen+label%3Agoal+label%3Aproj%3A{project}".into(),
    )
}

fn goal(number: u64, title: &str, gist: &str) -> super::GoalIssue {
    super::GoalIssue {
        number,
        title: title.into(),
        gist: gist.into(),
    }
}

#[tokio::test]
async fn list_renders_number_title_gist() {
    let md = view(vec![
        goal(10, "Цель А", "суть цели А"),
        goal(11, "Цель Б", ""),
    ])
    .list("uwuwu")
    .await
    .unwrap();
    assert_eq!(md, "- #10 Цель А — суть цели А\n- #11 Цель Б");
}

#[tokio::test]
async fn empty_goals_hint_carrier_and_filter_url() {
    let md = view(vec![]).list("origa").await.unwrap();
    assert!(md.contains("цели проекта не заведены"));
    assert!(md.contains("лейблы goal + proj:origa"));
    assert!(md.contains(
        "https://github.com/acme/hub/issues?q=is%3Aissue+is%3Aopen+label%3Agoal+label%3Aproj%3Aoriga"
    ));
}

#[tokio::test]
async fn unknown_project_is_honest_not_found() {
    let err = view(vec![]).list("ckii").await.unwrap_err();
    match err {
        ViewError::NotFound(msg) => {
            assert!(msg.contains("вне конфига"), "actual: {msg}")
        }
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[tokio::test]
async fn repo_error_passes_through_as_source() {
    struct Broken;
    #[async_trait]
    impl crate::goals::GoalsRepo for Broken {
        async fn open_goals(&self, _project: &str) -> crate::UsecaseResult<Vec<super::GoalIssue>> {
            Err(UsecaseError::Board("gh api: 502".into()))
        }
    }
    let v = GoalsView::new(
        Arc::new(Broken),
        vec!["uwuwu".into()],
        "u://{project}".into(),
    );
    let err = v.list("uwuwu").await.unwrap_err();
    assert!(matches!(err, ViewError::Source(_)));
}

#[test]
fn gist_takes_first_meaningful_line() {
    assert_eq!(gist_of("суть цели\n\nвторая строка"), "суть цели");
    assert_eq!(gist_of("   \n\t\nотступы норм"), "отступы норм");
    assert_eq!(gist_of(""), "");
    assert_eq!(gist_of("   "), "");
}

#[test]
fn gist_truncates_long_lines() {
    let long = "ц".repeat(200);
    let g = gist_of(&long);
    assert_eq!(g.chars().count(), 121); // 120 глифов + «…»
    assert!(g.ends_with('…'));
}
