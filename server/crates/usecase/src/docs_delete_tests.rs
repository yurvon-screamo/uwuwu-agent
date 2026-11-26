use super::*;
use crate::{BlobStore, DocSnapshot, UsecaseError};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Mutex;

type CallLog = Arc<Mutex<Vec<&'static str>>>;

struct RecordingWiki {
    docs: Mutex<HashMap<String, String>>,
    order: CallLog,
    delete_error: Option<UsecaseError>,
    post_check: PostCheck,
}

impl RecordingWiki {
    fn new(content: Option<&str>, order: CallLog) -> Self {
        let mut docs = HashMap::new();
        if let Some(body) = content {
            docs.insert("Projects/foo".to_string(), body.to_string());
        }
        Self {
            docs: Mutex::new(docs),
            order,
            delete_error: None,
            post_check: PostCheck::ConfirmedAbsent,
        }
    }

    fn calls(&self) -> Vec<&'static str> {
        self.order.lock().expect("calls").clone()
    }
}

#[async_trait]
impl WikiRepo for RecordingWiki {
    async fn read_doc(&self, _slug: &str) -> UsecaseResult<Option<String>> {
        Ok(None)
    }

    async fn read_doc_with_sha(&self, slug: &str) -> UsecaseResult<Option<DocSnapshot>> {
        self.order.lock().expect("calls").push("read");
        Ok(self
            .docs
            .lock()
            .expect("docs")
            .get(slug)
            .map(|content| DocSnapshot {
                content: content.clone(),
                blob_sha: "live-sha".to_string(),
            }))
    }

    async fn list_docs(&self, _prefix: &str) -> UsecaseResult<Vec<String>> {
        Ok(Vec::new())
    }

    async fn doc_shas(&self) -> UsecaseResult<Vec<(String, String)>> {
        Ok(Vec::new())
    }

    async fn write_doc(&self, _: &str, _: &str, _: &str) -> UsecaseResult<()> {
        Ok(())
    }

    async fn append_doc(&self, _: &str, _: &str, _: &str) -> UsecaseResult<usize> {
        Ok(0)
    }

    async fn delete_doc(
        &self,
        slug: &str,
        _old_blob_sha: &str,
        _message: &str,
    ) -> UsecaseResult<DocDeleteCommit> {
        self.order.lock().expect("calls").push("delete");
        if let Some(error) = &self.delete_error {
            return Err(error.clone());
        }
        self.docs.lock().expect("docs").remove(slug);
        Ok(DocDeleteCommit {
            commit_sha: "commit-sha".to_string(),
            post_check: self.post_check.clone(),
        })
    }
}

struct RecordingBlob {
    order: CallLog,
    bytes: Mutex<Vec<Vec<u8>>>,
    fail: bool,
}

#[async_trait]
impl BlobStore for RecordingBlob {
    async fn put(
        &self,
        _key: &uwuwu_domain::BlobKey,
        _ctype: &str,
        bytes: &[u8],
    ) -> UsecaseResult<()> {
        self.order.lock().expect("calls").push("backup");
        if self.fail {
            return Err(UsecaseError::Blob("s3 down".to_string()));
        }
        self.bytes.lock().expect("bytes").push(bytes.to_vec());
        Ok(())
    }

    fn presign_put_url(&self, _key: &uwuwu_domain::BlobKey, _ctype: &str) -> String {
        "https://blob.example/put".to_string()
    }

    fn presign_get_url(&self, _key: &uwuwu_domain::BlobKey) -> String {
        "https://blob.example/key".to_string()
    }
}

fn usecase(
    content: Option<&str>,
    blob_fail: bool,
    delete_error: Option<UsecaseError>,
    post_check: PostCheck,
) -> (
    DocsDeleteUseCase,
    std::sync::Arc<RecordingWiki>,
    std::sync::Arc<RecordingBlob>,
) {
    let order: CallLog = Arc::default();
    let mut record = RecordingWiki::new(content, order.clone());
    record.delete_error = delete_error;
    record.post_check = post_check;
    let wiki = std::sync::Arc::new(record);
    let blob = std::sync::Arc::new(RecordingBlob {
        order,
        bytes: Mutex::new(Vec::new()),
        fail: blob_fail,
    });
    (
        DocsDeleteUseCase::new(wiki.clone(), Arc::new(BlobUseCase::ready(blob.clone()))),
        wiki,
        blob,
    )
}

fn request() -> DeleteRequest {
    DeleteRequest {
        slug: "Projects/foo".to_string(),
        project: "uwuwu".to_string(),
        message: "delete: Projects/foo".to_string(),
        twins: vec!["Experience/uwuwu/Projects/foo".to_string()],
    }
}

#[test]
fn guard_demotes_suffix_twin_and_refuses_root_and_parent() {
    let live = vec![
        "Projects/foo".into(),
        "Experience/uwuwu/Projects/foo".into(),
    ];
    let verdict = guard("Projects/foo", &live);
    assert_eq!(verdict.refusal, None);
    assert_eq!(verdict.twins, vec!["Experience/uwuwu/Projects/foo"]);
    assert!(matches!(
        guard("cli/x", &live).refusal,
        Some(DeleteRefusal::RootPrefix(_))
    ));
    assert!(matches!(
        guard("Projects/foo/child", &["Projects/foo".into()]).refusal,
        Some(DeleteRefusal::ParentCollision(_))
    ));
}

#[tokio::test]
async fn delete_backs_up_live_doc_before_commit_and_keeps_twin() {
    let (uc, wiki, blob) = usecase(
        Some("---\ntitle: x\n---\nbody"),
        false,
        None,
        PostCheck::ConfirmedAbsent,
    );

    let receipt = uc.delete(request()).await.expect("delete");

    assert_eq!(wiki.calls(), vec!["read", "backup", "delete"]);
    assert_eq!(receipt.old_blob_sha, "live-sha");
    assert_eq!(receipt.commit_sha, "commit-sha");
    assert_eq!(receipt.path, "docs/Projects/foo.md");
    assert!(receipt.backup_key.starts_with("blobs/uwuwu/misc/"));
    assert_eq!(receipt.suffix_twins, vec!["Experience/uwuwu/Projects/foo"]);
    let saved = blob.bytes.lock().expect("bytes");
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0], b"---\ntitle: x\n---\nbody");
}

#[tokio::test]
async fn absent_doc_refuses_without_backup_or_commit() {
    let (uc, wiki, blob) = usecase(None, false, None, PostCheck::ConfirmedAbsent);

    let error = uc.delete(request()).await.expect_err("absent");

    assert!(matches!(error, UsecaseError::DocAbsent(_)), "{error}");
    assert_eq!(wiki.calls(), vec!["read"]);
    assert_eq!(blob.order.lock().expect("calls").len(), 1);
}

#[tokio::test]
async fn backup_failure_prevents_commit() {
    let (uc, wiki, blob) = usecase(Some("body"), true, None, PostCheck::ConfirmedAbsent);

    let error = uc.delete(request()).await.expect_err("backup failed");

    assert!(matches!(error, UsecaseError::BackupFailed(_)), "{error}");
    assert_eq!(wiki.calls(), vec!["read", "backup"]);
    assert_eq!(blob.order.lock().expect("calls").len(), 2);
}

#[tokio::test]
async fn racing_ref_is_refused_once_without_retry() {
    let (uc, wiki, _) = usecase(
        Some("body"),
        false,
        Some(UsecaseError::RaceRef("competing commit".into())),
        PostCheck::ConfirmedAbsent,
    );

    let error = uc.delete(request()).await.expect_err("race");

    assert!(matches!(error, UsecaseError::RaceRef(_)), "{error}");
    assert_eq!(wiki.calls(), vec!["read", "backup", "delete"]);
}

#[tokio::test]
async fn post_check_disagreement_is_carried_into_receipt() {
    let (uc, _, _) = usecase(
        Some("body"),
        false,
        None,
        PostCheck::Disagreement("path still in tree".into()),
    );

    let receipt = uc.delete(request()).await.expect("delete");

    assert_eq!(
        receipt.post_check,
        PostCheck::Disagreement("path still in tree".into())
    );
}
