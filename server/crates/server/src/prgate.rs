//! Маршрутизация PR-гейта по хосту PR-URL (#1269): грубая PR-форма —
//! домен (PrUrl::is_pull), вендор и строгость — адаптеры. PR-форм на
//! неуправляемых вендорах — Foreign: строка ждёт владельца, без bounce
//! (скоп-заметка #1286). Внешний PrGateway контура — эта обёртка.

use async_trait::async_trait;
use uwuwu_domain::PrUrl;
use uwuwu_usecase::{PrGateway, PrState, UsecaseError, UsecaseResult};

use crate::gitea::parse_gitea_pr_url;
use crate::github::parse_pr_url;

pub struct RoutedPrGateway<Gh, Gt> {
    gh: Gh,
    gitea: Gt,
}

impl<Gh, Gt> RoutedPrGateway<Gh, Gt>
where
    Gh: PrGateway,
    Gt: PrGateway,
{
    pub fn new(gh: Gh, gitea: Gt) -> Self {
        Self { gh, gitea }
    }
}

/// Маршрут PR-URL: вендор-парс адаптеров (строгие формы) → Foreign для
/// PR-формы чужого вендора (GHE-форма вне github.com) → веточная проба
/// GitHub для артефакт-URL и пустых ячеек (как до #1269).
fn route_for(pr_url: &str) -> Route {
    if parse_gitea_pr_url(pr_url).is_some() {
        return Route::Gitea;
    }
    if parse_pr_url(pr_url).is_some() {
        return Route::Github;
    }
    if PrUrl::parse(pr_url).is_some_and(|u| u.is_pull()) {
        return Route::Foreign;
    }
    Route::BranchProbe
}

enum Route {
    Github,
    Gitea,
    Foreign,
    BranchProbe,
}

#[async_trait]
impl<Gh, Gt> PrGateway for RoutedPrGateway<Gh, Gt>
where
    Gh: PrGateway,
    Gt: PrGateway,
{
    async fn probe(
        &self,
        project_repo: &str,
        pr_url: Option<&str>,
        branch: Option<&str>,
    ) -> UsecaseResult<PrState> {
        let Some(url) = pr_url.filter(|u| !u.is_empty()) else {
            return self.gh.probe(project_repo, None, branch).await;
        };
        match route_for(url) {
            Route::Github => self.gh.probe(project_repo, Some(url), branch).await,
            Route::Gitea => self.gitea.probe(project_repo, Some(url), None).await,
            Route::Foreign => {
                tracing::info!(
                    event = "prgate.foreign",
                    url = %url,
                    "PR-форм на неуправляемом вендоре — Foreign, строка ждёт владельца (#1269)"
                );
                Ok(PrState::Foreign)
            }
            Route::BranchProbe => self.gh.probe(project_repo, None, branch).await,
        }
    }

    async fn merge(&self, project_repo: &str, pr_url: &str) -> UsecaseResult<()> {
        match route_for(pr_url) {
            Route::Github => self.gh.merge(project_repo, pr_url).await,
            Route::Gitea => self.gitea.merge(project_repo, pr_url).await,
            // тик зовёт merge только после probe с зелёным Open — до Foreign
            // не доходит; BranchProbe-URL (артефакт) мержить нечего (#1155)
            Route::Foreign | Route::BranchProbe => Err(UsecaseError::Pr(format!(
                "мерж: PR-URL не управляется гейтвеем: {pr_url}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Фейковое плечо с журналом вызовов: пин делегирования диспетчера —
    /// маршрут дожен дойти до своего плеча с теми же аргументами, чужие
    /// плечи молчат (класс шва, на котором жил исходный баг #1269).
    /// Clone делит журнал (Arc<Mutex>) — фейк читается после move в routed.
    #[derive(Clone)]
    struct FakeGw {
        state: PrState,
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl FakeGw {
        fn new(state: PrState) -> Self {
            Self {
                state,
                calls: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl PrGateway for FakeGw {
        async fn probe(
            &self,
            _project_repo: &str,
            pr_url: Option<&str>,
            branch: Option<&str>,
        ) -> UsecaseResult<PrState> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("probe url={pr_url:?} branch={branch:?}"));
            Ok(self.state)
        }

        async fn merge(&self, _project_repo: &str, pr_url: &str) -> UsecaseResult<()> {
            self.calls.lock().unwrap().push(format!("merge {pr_url}"));
            Ok(())
        }
    }

    fn routed(
        gh_state: PrState,
        gitea_state: PrState,
    ) -> (RoutedPrGateway<FakeGw, FakeGw>, FakeGw, FakeGw) {
        let gh = FakeGw::new(gh_state);
        let gitea = FakeGw::new(gitea_state);
        (RoutedPrGateway::new(gh.clone(), gitea.clone()), gh, gitea)
    }

    #[tokio::test]
    async fn probe_gitea_url_delegates_to_gitea_only() {
        // Gitea-URL идёт в gitea-плечо (ветка вторична — None); gh молчит,
        // вердикт — gitea-плеча (Foreign vs gh Open — различимо)
        let (routed, gh, gitea) = routed(
            PrState::Open {
                mergeable: true,
                ci_green: true,
            },
            PrState::Foreign,
        );
        let state = routed
            .probe(
                "hub/o/r",
                Some("https://git.l4a.ru/o/r/pulls/7"),
                Some("work/task-1"),
            )
            .await
            .unwrap();
        assert_eq!(state, PrState::Foreign, "вердикт gitea-плеча");
        assert!(gh.calls().is_empty(), "gh не зовётся: {:?}", gh.calls());
        assert_eq!(
            gitea.calls(),
            vec!["probe url=Some(\"https://git.l4a.ru/o/r/pulls/7\") branch=None"]
        );
    }

    #[tokio::test]
    async fn probe_github_url_delegates_to_gh_with_branch() {
        // github-URL — gh-плечо с веткой (probe_pull 404 → веточный фоллбэк
        // остаётся в gh); gitea молчит, вердикт — gh-плеча
        let (routed, gh, gitea) = routed(PrState::Pending, PrState::Foreign);
        let state = routed
            .probe(
                "hub/o/r",
                Some("https://github.com/o/r/pull/5"),
                Some("work/task-1"),
            )
            .await
            .unwrap();
        assert_eq!(state, PrState::Pending, "вердикт gh-плеча");
        assert_eq!(
            gh.calls(),
            vec!["probe url=Some(\"https://github.com/o/r/pull/5\") branch=Some(\"work/task-1\")"]
        );
        assert!(gitea.calls().is_empty());
    }

    #[tokio::test]
    async fn probe_without_url_falls_back_to_gh_branch_probe() {
        // пустая PR-ячейка — веточная проба GitHub (как до #1269)
        let (routed, gh, gitea) = routed(PrState::Missing, PrState::Foreign);
        let state = routed
            .probe("hub/o/r", None, Some("work/task-1"))
            .await
            .unwrap();
        assert_eq!(state, PrState::Missing);
        assert_eq!(
            gh.calls(),
            vec!["probe url=None branch=Some(\"work/task-1\")"]
        );
        assert!(gitea.calls().is_empty());
    }

    #[tokio::test]
    async fn probe_artifact_url_falls_back_to_gh_branch_probe() {
        // артефакт-URL — не PR-форма: веточная проба GitHub без URL
        let (routed, gh, gitea) = routed(PrState::Missing, PrState::Foreign);
        let state = routed
            .probe(
                "hub/o/r",
                Some("https://github.com/o/r/issues/5"),
                Some("work/task-1"),
            )
            .await
            .unwrap();
        assert_eq!(state, PrState::Missing);
        assert_eq!(
            gh.calls(),
            vec!["probe url=None branch=Some(\"work/task-1\")"]
        );
        assert!(gitea.calls().is_empty());
    }

    #[tokio::test]
    async fn probe_foreign_url_is_decided_without_any_gateway_call() {
        // PR-форма чужого вендора — вердикт диспетчера: Foreign без проб
        let (routed, gh, gitea) = routed(PrState::Missing, PrState::Missing);
        let state = routed
            .probe(
                "hub/o/r",
                Some("https://ghe.example.com/o/r/pull/5"),
                Some("work/task-1"),
            )
            .await
            .unwrap();
        assert_eq!(state, PrState::Foreign);
        assert!(gh.calls().is_empty());
        assert!(gitea.calls().is_empty());
    }

    #[tokio::test]
    async fn merge_delegates_by_vendor() {
        // мерж маршрутизируется по вендору: github → gh, gitea-форма → gitea
        let (routed, gh, gitea) = routed(PrState::Missing, PrState::Missing);
        routed
            .merge("o/r", "https://github.com/o/r/pull/5")
            .await
            .unwrap();
        routed
            .merge("o/r", "https://git.l4a.ru/o/r/pulls/7")
            .await
            .unwrap();
        assert_eq!(gh.calls(), vec!["merge https://github.com/o/r/pull/5"]);
        assert_eq!(gitea.calls(), vec!["merge https://git.l4a.ru/o/r/pulls/7"]);
    }

    #[tokio::test]
    async fn merge_foreign_url_errors_without_network() {
        // merge до Foreign не доходит (probe отфильтровывает), defensive Err
        let (routed, gh, gitea) = routed(PrState::Missing, PrState::Missing);
        let err = routed
            .merge("o/r", "https://ghe.example.com/o/r/pull/5")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("не управляется"));
        assert!(gh.calls().is_empty());
        assert!(gitea.calls().is_empty());
    }

    #[test]
    fn routes_by_host_and_shape() {
        // github-строгая форма — GH-адаптер
        assert!(matches!(
            route_for("https://github.com/o/r/pull/5"),
            Route::Github
        ));
        // Gitea-форма любого хоста — Gitea-адаптер (токены решат пробу/Foreign)
        assert!(matches!(
            route_for("https://git.l4a.ru/ek/ai-generator/pulls/74"),
            Route::Gitea
        ));
        assert!(matches!(
            route_for("https://git.true-wash.com/a/b/pulls/9"),
            Route::Gitea
        ));
        // PR-форма чужого вендора (GHE вне github.com) — Foreign
        assert!(matches!(
            route_for("https://ghe.example.com/o/r/pull/5"),
            Route::Foreign
        ));
        // артефакт-URL — веточная проба (существующее поведение)
        assert!(matches!(
            route_for("https://github.com/o/r/issues/5"),
            Route::BranchProbe
        ));
        assert!(matches!(
            route_for("https://github.com/o/r/blob/main/docs/x.md"),
            Route::BranchProbe
        ));
    }
}
