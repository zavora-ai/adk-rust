//! `adk-rust website` (spec 3.5 §7.17, §7.11, §7.21, §8.1): `new` scaffolds a site, serves
//! its edge and kernel with the web home beside them and prints the setup code and the
//! owner's key steps; the setup code pairs a device; `status`, `charter`, `review` and `jobs`
//! read the site's stores; promoting and demoting a job is the owner's decision, granting
//! and revoking its standing rules; `--agency` serves every listed site from one harness.

#![cfg(all(unix, feature = "website"))]

use std::io::{BufRead as _, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::Value;

/// A running `adk-rust website new`, killed when dropped.
struct Running {
    child: Child,
    lines: Vec<String>,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Running {
    /// The address printed after `label`, such as `edge`.
    fn url(&self, label: &str) -> String {
        self.lines
            .iter()
            .find_map(|line| line.trim_start().strip_prefix(label).map(str::trim_start))
            .and_then(|rest| rest.split_whitespace().next())
            .unwrap_or_else(|| panic!("no {label} in {:#?}", self.lines))
            .to_string()
    }
}

fn command(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_adk-rust"));
    command
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("USER", "amina")
        .env("RUST_LOG", "off");
    command
}

/// Starts `website new` with `args` and reads its output until it is ready.
fn start(home: &Path, args: &[&str]) -> Running {
    let mut child = command(home)
        .args(["website", "new"])
        .args(args)
        .args([
            "--edge",
            "127.0.0.1:0",
            "--kernel",
            "127.0.0.1:0",
            "--home",
            "127.0.0.1:0",
            "--no-open",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("adk-rust starts");
    let stdout = child.stdout.take().unwrap();
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if send.send(line).is_err() {
                break;
            }
        }
    });
    let mut running = Running { child, lines: Vec::new() };
    loop {
        match receive.recv_timeout(Duration::from_secs(120)) {
            Ok(line) => {
                let ready = line.starts_with("Press Ctrl+C");
                running.lines.push(line);
                if ready {
                    return running;
                }
            }
            Err(_) => {
                use std::io::Read as _;
                let _ = running.child.kill();
                let mut stderr = String::new();
                if let Some(mut pipe) = running.child.stderr.take() {
                    let _ = pipe.read_to_string(&mut stderr);
                }
                panic!("website new did not start:\n{}\n{stderr}", running.lines.join("\n"));
            }
        }
    }
}

fn run(home: &Path, args: &[&str]) -> (bool, String, String) {
    let output = command(home).args(args).stdin(Stdio::null()).output().expect("adk-rust runs");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

async fn get(url: &str) -> (u16, String) {
    let response = reqwest::get(url).await.unwrap();
    (response.status().as_u16(), response.text().await.unwrap())
}

/// Commits the project's draft charter by its owner, as onboarding's signing does.
async fn commit_charter(site: &Path) {
    use adk_kernel::charter::{CharterStore as _, CommitDecision, RoleApproval};
    let paths = adk_website::deploy::ProjectPaths::new(site);
    let stores =
        adk_website::deploy::KernelStores::open(&paths.database("kernel"), "owner").await.unwrap();
    let draft: awp_types::Charter =
        serde_json::from_slice(&std::fs::read(site.join("charter.draft.json")).unwrap()).unwrap();
    let now = chrono::Utc::now();
    let mut decision = CommitDecision::new("onboarding-signing", now);
    decision.approvals.push(RoleApproval::new(
        awp_types::Role::Owner,
        adk_core::principal::SubjectRef::human("owner"),
        now,
    ));
    stores.charters.commit(draft, decision).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn new_serves_a_site_whose_setup_code_pairs_a_device_and_the_owner_runs_it() {
    let home = tempfile::tempdir().unwrap();
    let site = home.path().join("bakery");
    let site_arg = site.display().to_string();
    let running = start(
        home.path(),
        &[
            &site_arg,
            "--kind",
            "business",
            "--name",
            "Juniper Bakery",
            "--domain",
            "bakery.example",
        ],
    );
    let printed = running.lines.join("\n");
    for file in ["site.toml", "charter.draft.json", "capabilities.json", "public-terms.json"] {
        assert!(site.join(file).exists(), "{file}");
    }
    assert!(printed.contains("Juniper Bakery (bakery.example) is running"), "{printed}");
    assert!(printed.contains("awp keys rotate --site"), "{printed}");
    assert!(printed.contains("--scope release --key"), "{printed}");
    assert!(!site.join("data/keys/root.jwk").exists(), "no root key in the project (D-11)");
    let code = printed
        .lines()
        .find_map(|line| line.strip_prefix("Setup code: "))
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("no setup code in {printed}"))
        .to_string();

    // The edge serves the holding page until the launch.
    let (status, page) = get(&format!("{}/", running.url("edge"))).await;
    assert_eq!(status, 200);
    assert!(page.contains("Juniper Bakery"), "{page}");

    // The home's setup screen, and the code pairs a device once (A-I20).
    let origin = running.url("home").trim_end_matches("/ui/website").to_string();
    let (status, setup) = get(&format!("{origin}/api/surface/website/setup")).await;
    assert_eq!(status, 200, "{setup}");
    let client = reqwest::Client::new();
    let claim = |code: String| {
        client
            .post(format!("{origin}/api/surface/website/setup/claim"))
            .json(&serde_json::json!({ "code": code, "device": "Amina's phone" }))
            .send()
    };
    let claimed = claim(code.clone()).await.unwrap();
    assert!(claimed.status().is_success(), "{:?}", claimed.text().await);
    let token = claimed.json::<Value>().await.unwrap()["token"].as_str().unwrap().to_string();
    let again = claim(code).await.unwrap();
    assert!(!again.status().is_success(), "a setup code pairs one device");

    // The paired device starts onboarding; the home's review view composes the week.
    let started = client
        .post(format!("{origin}/api/surface/website/onboarding"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "type": "start", "from": "nothing", "name": "Juniper Bakery" }))
        .send()
        .await
        .unwrap();
    assert!(started.status().is_success(), "{:?}", started.text().await);
    let composed = client
        .post(format!("{origin}/api/surface/website/review"))
        .json(&serde_json::json!({ "type": "compose" }))
        .send()
        .await
        .unwrap();
    assert!(composed.status().is_success(), "{:?}", composed.text().await);
    let page: Value = composed.json().await.unwrap();
    assert_eq!(page["canCompose"], true, "{page}");
    assert!(page["current"]["summary"].as_str().is_some_and(|s| s.starts_with("Your week")));
    drop(running);

    // After a restart the paired device is still the one onboarding admits, and onboarding
    // resumes where the owner left it (spec 3.5 §8.2, A-I20).
    assert!(site.join("data/desk.json").exists(), "the desk saved its state");
    let restarted = start(home.path(), &[&site_arg]);
    let origin = restarted.url("home").trim_end_matches("/ui/website").to_string();
    let resumed = client
        .get(format!("{origin}/api/surface/website/onboarding"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    let status = resumed.status();
    let view = resumed.text().await.unwrap();
    assert!(status.is_success(), "{view}");
    assert!(view.contains("Juniper Bakery"), "{view}");
    drop(restarted);

    // Status, charter, review and jobs read the site's stores.
    let (ok, status, err) = run(home.path(), &["website", "status", "--site", &site_arg, "--json"]);
    assert!(ok, "{err}");
    let status: Value = serde_json::from_str(&status).unwrap();
    assert_eq!(status["domain"], "bakery.example");
    assert_eq!(status["pairedDevices"], 1);
    assert_eq!(status["launched"], false);
    let (ok, charter, err) = run(home.path(), &["website", "charter", "--site", &site_arg]);
    assert!(ok, "{err}");
    assert!(charter.contains("No charter is in force yet"), "{charter}");

    commit_charter(&site).await;
    let (ok, charter, err) = run(home.path(), &["website", "charter", "--site", &site_arg]);
    assert!(ok, "{err}");
    assert!(charter.contains("Charter version 1 of bakery.example"), "{charter}");
    let (ok, review, err) = run(home.path(), &["website", "review", "--site", &site_arg, "--json"]);
    assert!(ok, "{err}");
    let review: Value = serde_json::from_str(&review).unwrap();
    assert_eq!(review["charterVersion"], 1);

    let (ok, jobs, err) = run(home.path(), &["website", "jobs", "--site", &site_arg, "--json"]);
    assert!(ok, "{err}");
    let jobs: Vec<Value> = serde_json::from_str(&jobs).unwrap();
    // A job the charter's authority starts at Propose, which the ladder registered.
    let draft: Value =
        serde_json::from_slice(&std::fs::read(site.join("charter.draft.json")).unwrap()).unwrap();
    let job = draft["authority"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|authority| authority["level"] == "propose")
        .filter_map(|authority| authority["job"].as_str())
        .find(|job| jobs.iter().any(|j| j["job"] == *job && j["level"] == "propose"))
        .expect("a job at propose")
        .to_string();

    // Without a terminal and without --yes, nothing changes.
    let (ok, _, err) = run(home.path(), &["website", "jobs", "--site", &site_arg, "promote", &job]);
    assert!(!ok && err.contains("--yes"), "{err}");
    // A promotion is the owner's decision; a person who is not an owner is refused.
    let (ok, _, err) = run(
        home.path(),
        &["website", "jobs", "--site", &site_arg, "--owner", "stranger", "promote", &job, "--yes"],
    );
    assert!(!ok, "{err}");
    let (ok, out, err) =
        run(home.path(), &["website", "jobs", "--site", &site_arg, "promote", &job, "--yes"]);
    assert!(ok, "{err}");
    assert!(out.contains(&format!("Promoted `{job}` to act_with_approval")), "{out}");
    let (ok, out, err) =
        run(home.path(), &["website", "jobs", "--site", &site_arg, "demote", &job, "--yes"]);
    assert!(ok, "{err}");
    assert!(out.contains(&format!("Demoted `{job}` to propose")), "{out}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agency_serves_every_listed_site_from_one_harness() {
    let home = tempfile::tempdir().unwrap();
    let agency = home.path().join("agency");
    let agency_arg = agency.display().to_string();
    let first = start(home.path(), &[&agency_arg, "--agency", "--domain", "bakery.example"]);
    assert!(first.lines.join("\n").contains("agency  1 site(s): bakery.example"));
    drop(first);
    let second = start(
        home.path(),
        &[&agency_arg, "--agency", "--domain", "notes.example", "--kind", "personal"],
    );
    let printed = second.lines.join("\n");
    assert!(printed.contains("agency  2 site(s)"), "{printed}");
    let listed: adk_website::deploy::AgencyFile =
        toml::from_str(&std::fs::read_to_string(agency.join("agency.toml")).unwrap()).unwrap();
    assert_eq!(listed.sites.len(), 2);
    assert!(agency.join("notes.example/site.toml").exists());
    // The edge sends a request only to the site its host names (A-I22).
    let edge = second.url("edge");
    let client = reqwest::Client::new();
    for host in ["bakery.example", "notes.example"] {
        let page = client.get(format!("{edge}/")).header("host", host).send().await.unwrap();
        assert_eq!(page.status().as_u16(), 200, "{host}");
    }
    // The owner pairs the agency once; every listed site's commands read the agency's kernel
    // database, which the served sites run on (A-I22).
    let code = printed
        .lines()
        .find_map(|line| line.strip_prefix("Setup code: "))
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("no setup code in {printed}"))
        .to_string();
    let origin = second.url("home").trim_end_matches("/ui/website").to_string();
    let claimed = client
        .post(format!("{origin}/api/surface/website/setup/claim"))
        .json(&serde_json::json!({ "code": code, "device": "Agency laptop" }))
        .send()
        .await
        .unwrap();
    assert!(claimed.status().is_success(), "{:?}", claimed.text().await);
    drop(second);
    for site in ["bakery.example", "notes.example"] {
        let dir = agency.join(site).display().to_string();
        let (ok, status, err) = run(home.path(), &["website", "status", "--site", &dir, "--json"]);
        assert!(ok, "{err}");
        let status: Value = serde_json::from_str(&status).unwrap();
        assert_eq!(status["pairedDevices"], 1, "{site} reads the agency's kernel database");
    }
}
