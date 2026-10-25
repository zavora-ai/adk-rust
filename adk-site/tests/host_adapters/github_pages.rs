//! `GitPagesHost` against local bare repositories with the real `git`, and against a
//! GitHub repository when one is configured.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use adk_site::host::conformance::{self, Fixture};
use adk_site::host::github_pages::GitPagesHost;
use adk_site::host::{HostTarget, SiteHost};
use adk_site::{Result, SiteError};

use crate::support::credential;

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git").arg("--git-dir").arg(dir).args(args).output().expect("git");
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn bare(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let status = Command::new("git")
        .args(["init", "--bare", "--quiet"])
        .arg(dir)
        .status()
        .expect("git init");
    assert!(status.success());
    dir.to_path_buf()
}

fn host(root: &Path, name: &str) -> (GitPagesHost, PathBuf) {
    let remote = bare(&root.join(format!("{name}.git")));
    let host = GitPagesHost::new(remote.display().to_string(), "gh-pages")
        .with_work_dir(root.join(format!("{name}-cache")))
        .with_committer("Site Publisher", "publisher@example.com");
    (host, remote)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_git_pages_branch_passes_the_conformance_suite() {
    let root = tempfile::tempdir().unwrap();
    let next = std::sync::atomic::AtomicUsize::new(0);
    conformance::run_all(|| {
        let n = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Arc::new(host(root.path(), &format!("site-{n}")).0) as Arc<dyn SiteHost>
    })
    .await;
}

#[tokio::test]
async fn each_release_is_one_commit_with_host_files_and_previews_on_their_branch() -> Result<()> {
    let root = tempfile::tempdir().unwrap();
    let (host, remote) = host(root.path(), "site");
    let host = host.with_cname("www.example.com").with_preview_branch("gh-pages-preview");
    let mut fixture = Fixture::new().await;
    let files = |v: &str| {
        vec![("/index.html", format!("<p>{v}</p>").into_bytes()), ("/old.html", b"old".to_vec())]
    };
    let a = fixture.release(&files("a"), Vec::new(), false).await;
    host.publish(&a, HostTarget::Production).await?;
    let b = fixture.release(&[("/index.html", b"<p>b</p>".to_vec())], Vec::new(), false).await;
    let report = host.publish(&b, HostTarget::Production).await?;
    assert_eq!(report.removed, vec!["/old.html".to_string()]);

    let log = git(&remote, &["log", "--format=%s|%an <%ae>", "gh-pages"]);
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 2, "{log}");
    assert!(lines[0].starts_with("adk-site release 2 sha256:"), "{log}");
    assert!(lines[0].ends_with("|Site Publisher <publisher@example.com>"), "{log}");
    let tree = git(&remote, &["ls-tree", "-r", "--name-only", "gh-pages"]);
    assert!(tree.lines().any(|l| l == ".nojekyll") && tree.lines().any(|l| l == "CNAME"), "{tree}");
    assert!(!tree.lines().any(|l| l == "old.html"), "{tree}");
    assert_eq!(git(&remote, &["show", "gh-pages:CNAME"]), "www.example.com\n");

    let preview =
        fixture.release(&[("/index.html", b"<p>next</p>".to_vec())], Vec::new(), false).await;
    host.publish(&preview, HostTarget::Preview).await?;
    assert_eq!(git(&remote, &["show", "gh-pages-preview:index.html"]), "<p>next</p>");
    assert_eq!(host.fetch("/").await?.as_deref(), Some(b"<p>b</p>".as_slice()));
    assert!(
        git(&remote, &["show", "gh-pages-preview:.well-known/awp/log/checkpoint"])
            .contains("conformance.example")
    );
    Ok(())
}

#[tokio::test]
async fn previews_need_a_branch_and_unknown_releases_are_not_rolled_back_to() -> Result<()> {
    let root = tempfile::tempdir().unwrap();
    let (host, _) = host(root.path(), "site");
    let mut fixture = Fixture::new().await;
    let a = fixture.release(&[("/index.html", b"<p>a</p>".to_vec())], Vec::new(), false).await;
    let error = host.publish(&a, HostTarget::Preview).await.unwrap_err();
    assert!(error.to_string().contains("with_preview_branch"), "{error}");
    assert!(matches!(
        host.rollback(&a.manifest).await,
        Err(SiteError::ReleaseNotFound { seq: 1, .. })
    ));
    host.publish(&a, HostTarget::Production).await?;
    assert!(matches!(host.publish(&a, HostTarget::Production).await, Err(SiteError::Sequence(_))));
    assert!(host.fetch("/../x").await.is_err());
    Ok(())
}

#[tokio::test]
async fn a_second_publisher_builds_on_the_branch_it_finds() -> Result<()> {
    let root = tempfile::tempdir().unwrap();
    let (first, remote) = host(root.path(), "site");
    let second = GitPagesHost::new(remote.display().to_string(), "gh-pages")
        .with_work_dir(root.path().join("other-cache"));
    let mut fixture = Fixture::new().await;
    let a = fixture.release(&[("/index.html", b"<p>a</p>".to_vec())], Vec::new(), false).await;
    first.publish(&a, HostTarget::Production).await?;
    let b = fixture.release(&[("/index.html", b"<p>b</p>".to_vec())], Vec::new(), false).await;
    second.publish(&b, HostTarget::Production).await?;
    assert_eq!(
        first.fetch("/").await?.as_deref(),
        Some(b"<p>b</p>".as_slice()),
        "the first cache follows the remote"
    );
    let c = fixture.release(&[("/index.html", b"<p>c</p>".to_vec())], Vec::new(), false).await;
    first.publish(&c, HostTarget::Production).await?;
    assert_eq!(git(&remote, &["rev-list", "--count", "gh-pages"]).trim(), "3");
    Ok(())
}

/// Runs the conformance suite against a GitHub repository, one branch per check, each
/// deleted afterwards.
///
/// ```sh
/// ADK_SITE_GIT_REMOTE=https://github.com/<owner>/<repo>.git ADK_SITE_GIT_TOKEN=<token with contents:write> \
///   cargo nextest run -p adk-site --features host-github-pages --run-ignored only github_repository
/// ```
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a GitHub repository: set ADK_SITE_GIT_REMOTE and ADK_SITE_GIT_TOKEN"]
async fn github_repository_passes_the_conformance_suite() {
    let remote = std::env::var("ADK_SITE_GIT_REMOTE").expect("set ADK_SITE_GIT_REMOTE");
    let token = std::env::var("ADK_SITE_GIT_TOKEN").expect("set ADK_SITE_GIT_TOKEN");
    let root = tempfile::tempdir().unwrap();
    let run = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let branches: Vec<String> = (0..8).map(|n| format!("adk-site-conformance-{run}-{n}")).collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let hosts: Vec<Arc<GitPagesHost>> = branches
        .iter()
        .map(|branch| {
            Arc::new(
                GitPagesHost::new(remote.clone(), branch.clone())
                    .with_credential(credential(&remote, &token))
                    .with_work_dir(root.path().join(branch)),
            )
        })
        .collect();
    let result = tokio::spawn({
        let hosts = hosts.clone();
        async move {
            conformance::run_all(|| {
                hosts[next.fetch_add(1, std::sync::atomic::Ordering::SeqCst)].clone()
                    as Arc<dyn SiteHost>
            })
            .await;
        }
    })
    .await;
    for (host, branch) in hosts.iter().zip(&branches) {
        let _ = Command::new("git")
            .arg("--git-dir")
            .arg(host.work_dir())
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "http.extraHeader")
            .env(
                "GIT_CONFIG_VALUE_0",
                format!(
                    "Authorization: Basic {}",
                    base64::Engine::encode(
                        &base64::engine::general_purpose::STANDARD,
                        format!("x-access-token:{token}")
                    )
                ),
            )
            .args(["push", "--quiet", &remote, &format!(":refs/heads/{branch}")])
            .status();
    }
    result.expect("the conformance suite passed on GitHub");
}
