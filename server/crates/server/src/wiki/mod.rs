//! Вики-адаптеры: git-клон (ff-only push) и GitHub Contents REST.

mod github_wiki;
mod gitwiki;

pub use github_wiki::GitHubWiki;
pub use gitwiki::GitWikiWriter;
