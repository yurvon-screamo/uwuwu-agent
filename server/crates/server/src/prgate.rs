//! Маршрутизация PR-гейта по хосту PR-URL (#1269): грубая PR-форма —
//! домен (PrUrl::is_pull), вендор и строгость — адаптеры. PR-форм на
//! неуправляемых вендорах — Foreign: строка ждёт владельца, без bounce
//! (скоп-заметка #1286). Внешний PrGateway контура — эта обёртка.

use async_trait::async_trait;
use uwuwu_domain::PrUrl;
use uwuwu_usecase::{PrGateway, PrState, UsecaseError, UsecaseResult};

use crate::gitea::{parse_gitea_pr_url, GiteaPrGateway};
use crate::github::{parse_pr_url, GhPrGateway};

pub struct RoutedPrGateway {
    gh: GhPrGateway,
    gitea: GiteaPrGateway,
}

impl RoutedPrGateway {
    pub fn new(gh: GhPrGateway, gitea: GiteaPrGateway) -> Self {
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
impl PrGateway for RoutedPrGateway {
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
    use std::collections::HashMap;

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

    #[tokio::test]
    async fn merge_foreign_url_errors_without_network() {
        // merge до Foreign не доходит (probe отфильтровывает), defensive Err
        let routed = RoutedPrGateway::new(
            GhPrGateway::new("t".into()),
            GiteaPrGateway::new(HashMap::new()),
        );
        let err = routed
            .merge("o/r", "https://ghe.example.com/o/r/pull/5")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("не управляется"));
    }
}
