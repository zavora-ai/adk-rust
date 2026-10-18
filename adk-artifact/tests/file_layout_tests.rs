//! On-disk layout of `FileArtifactService`: crash-safe writes and the earlier layout.

use adk_artifact::{
    ArtifactService, DeleteRequest, FileArtifactService, ListRequest, LoadRequest, SaveRequest,
    VersionsRequest,
};
use adk_core::Part;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn save(session: &str, name: &str, text: &str) -> SaveRequest {
    SaveRequest {
        app_name: "app".to_string(),
        user_id: "u1".to_string(),
        session_id: session.to_string(),
        file_name: name.to_string(),
        part: Part::Text { text: text.to_string() },
        version: None,
    }
}

async fn load_text(
    service: &FileArtifactService,
    session: &str,
    name: &str,
    version: Option<i64>,
) -> String {
    let part = service
        .load(LoadRequest {
            app_name: "app".to_string(),
            user_id: "u1".to_string(),
            session_id: session.to_string(),
            file_name: name.to_string(),
            version,
        })
        .await
        .unwrap()
        .part;
    match part {
        Part::Text { text } => text,
        other => panic!("expected text, got {other:?}"),
    }
}

async fn versions(service: &FileArtifactService, session: &str, name: &str) -> Vec<i64> {
    service
        .versions(VersionsRequest {
            app_name: "app".to_string(),
            user_id: "u1".to_string(),
            session_id: session.to_string(),
            file_name: name.to_string(),
        })
        .await
        .map(|response| response.versions)
        .unwrap_or_default()
}

async fn list(service: &FileArtifactService, session: &str) -> Vec<String> {
    service
        .list(ListRequest {
            app_name: "app".to_string(),
            user_id: "u1".to_string(),
            session_id: session.to_string(),
        })
        .await
        .unwrap()
        .file_names
}

fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Writes a version the way releases before the injective encoding did.
fn write_legacy(base: &Path, scope: &str, dir_name: &str, version: i64, text: &str) {
    let dir = base.join("app").join("u1").join(scope).join(dir_name);
    std::fs::create_dir_all(&dir).unwrap();
    let payload = serde_json::to_vec(&Part::Text { text: text.to_string() }).unwrap();
    std::fs::write(dir.join(format!("v{version}.json")), payload).unwrap();
}

#[tokio::test]
async fn a_save_leaves_only_complete_version_files() {
    let base = tempfile::tempdir().unwrap();
    let service = FileArtifactService::new(base.path()).unwrap();
    service.save(save("s1", "notes:today", "one")).await.unwrap();
    service.save(save("s1", "notes:today", "two")).await.unwrap();

    let dir = base.path().join("app/u1/s1/notes%3Atoday");
    assert_eq!(entries(&dir), vec!["v1.json", "v2.json"]);
    assert_eq!(load_text(&service, "s1", "notes:today", None).await, "two");
}

const CRASH_DIR_ENV: &str = "ADK_ARTIFACT_CRASH_WRITER_DIR";
const CRASH_BYTES: usize = 96 << 20;

/// The writer process of `a_writer_killed_mid_save_leaves_earlier_versions_intact`; does
/// nothing unless that test starts it.
#[tokio::test]
async fn crash_writer_child() {
    let Ok(dir) = std::env::var(CRASH_DIR_ENV) else {
        return;
    };
    let service = FileArtifactService::new(dir).unwrap();
    service.save(save("s1", "big.txt", &"x".repeat(CRASH_BYTES))).await.unwrap();
}

#[tokio::test]
async fn a_writer_killed_mid_save_leaves_earlier_versions_intact() {
    let base = tempfile::tempdir().unwrap();
    let service = FileArtifactService::new(base.path()).unwrap();
    service.save(save("s1", "big.txt", "complete")).await.unwrap();
    let dir = base.path().join("app/u1/s1/big.txt");
    let before = entries(&dir).len();

    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["crash_writer_child", "--exact", "--nocapture"])
        .env(CRASH_DIR_ENV, base.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let started = Instant::now();
    // Kill the writer as soon as its first file appears, long before 96 MiB are written.
    while entries(&dir).len() == before {
        assert!(child.try_wait().unwrap().is_none(), "the writer exited before it was killed");
        assert!(started.elapsed() < Duration::from_secs(60), "the writer never started");
        std::thread::sleep(Duration::from_micros(100));
    }
    child.kill().unwrap();
    child.wait().unwrap();

    assert_eq!(versions(&service, "s1", "big.txt").await, vec![1]);
    assert_eq!(load_text(&service, "s1", "big.txt", None).await, "complete");
}

#[tokio::test]
async fn an_interrupted_save_leaves_the_previous_version_loadable() {
    let base = tempfile::tempdir().unwrap();
    let service = FileArtifactService::new(base.path()).unwrap();
    service.save(save("s1", "report.txt", "complete")).await.unwrap();

    // A crash mid-save leaves its partially written temporary file behind.
    let dir = base.path().join("app/u1/s1/report.txt");
    std::fs::write(dir.join(".save-1-2-3.tmp"), b"{\"text\":\"trunc").unwrap();

    assert_eq!(versions(&service, "s1", "report.txt").await, vec![1]);
    assert_eq!(load_text(&service, "s1", "report.txt", None).await, "complete");
    let next = service.save(save("s1", "report.txt", "after the crash")).await.unwrap();
    assert_eq!(next.version, 2);
    assert_eq!(load_text(&service, "s1", "report.txt", None).await, "after the crash");
}

#[tokio::test]
async fn user_scoped_artifacts_live_in_the_user_directory() {
    let base = tempfile::tempdir().unwrap();
    let service = FileArtifactService::new(base.path()).unwrap();
    service.save(save("s1", "user:prefs", "dark mode")).await.unwrap();

    assert_eq!(entries(&base.path().join("app/u1")), vec!["user"]);
    assert_eq!(entries(&base.path().join("app/u1/user")), vec!["user%3Aprefs"]);

    // A session named after the old user scope directory is an ordinary session.
    service.save(save("_user_scoped_", "draft.txt", "mine")).await.unwrap();
    assert_eq!(list(&service, "s2").await, vec!["user:prefs"]);
    assert_eq!(list(&service, "_user_scoped_").await, vec!["draft.txt", "user:prefs"]);
}

#[tokio::test]
async fn artifacts_in_the_earlier_layout_stay_loadable() {
    let base = tempfile::tempdir().unwrap();
    write_legacy(base.path(), "s1", "report__q1", 1, "quarterly");
    write_legacy(base.path(), "s1", "plain.txt", 1, "plain");
    write_legacy(base.path(), "_user_scoped_", "user__prefs", 1, "prefs v1");
    write_legacy(base.path(), "_user_scoped_", "user__prefs", 2, "prefs v2");
    let service = FileArtifactService::new(base.path()).unwrap();

    assert_eq!(load_text(&service, "s1", "report:q1", None).await, "quarterly");
    assert_eq!(load_text(&service, "s1", "plain.txt", None).await, "plain");
    assert_eq!(load_text(&service, "s9", "user:prefs", None).await, "prefs v2");
    assert_eq!(load_text(&service, "s9", "user:prefs", Some(1)).await, "prefs v1");
    assert_eq!(list(&service, "s1").await, vec!["plain.txt", "report:q1", "user:prefs"]);

    // New versions of a legacy artifact continue its numbering in the current layout.
    let next = service.save(save("s1", "user:prefs", "prefs v3")).await.unwrap();
    assert_eq!(next.version, 3);
    assert_eq!(versions(&service, "s1", "user:prefs").await, vec![3, 2, 1]);
    assert_eq!(load_text(&service, "s1", "user:prefs", None).await, "prefs v3");
    assert_eq!(list(&service, "s1").await, vec!["plain.txt", "report:q1", "user:prefs"]);

    // An explicit version taken in the earlier layout is not replaced either.
    let mut explicit = save("s1", "report:q1", "replacement");
    explicit.version = Some(1);
    assert!(service.save(explicit).await.unwrap_err().is_conflict());

    // Deleting all versions removes both layouts.
    service
        .delete(DeleteRequest {
            app_name: "app".to_string(),
            user_id: "u1".to_string(),
            session_id: "s1".to_string(),
            file_name: "user:prefs".to_string(),
            version: None,
        })
        .await
        .unwrap();
    assert!(versions(&service, "s1", "user:prefs").await.is_empty());
    assert_eq!(list(&service, "s1").await, vec!["plain.txt", "report:q1"]);
}
