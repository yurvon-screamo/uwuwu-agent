//! Вики-адаптеры: git-клон (ff-only push) и GitHub Contents REST.

mod github_wiki;
mod gitwiki;

pub(crate) use github_wiki::encode_path;
pub use github_wiki::GitHubWiki;
pub use gitwiki::GitWikiWriter;
