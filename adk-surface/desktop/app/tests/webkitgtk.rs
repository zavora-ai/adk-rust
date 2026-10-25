//! The shell on WebKitGTK, driven through `tauri-driver` (spec 3.3 §5.5: "Tauri 2, after a
//! WebKitGTK test on Linux"; §12 phase 5). Ignored: it needs Linux, `tauri-driver`,
//! `WebKitWebDriver`, a display (`xvfb-run`) and a kernel binary; `.github/workflows/desktop.yml`
//! runs it.
//!
//! Each test starts its kernel itself, so it can stop and start it again, and kills it on every
//! exit path. The shell reads its kernel from `ADK_DESKTOP_SERVER` (and its token from
//! `ADK_SERVE_TOKEN`), set where `tauri-driver` starts, since the driver starts the shell.
//!
//! | Variable | Meaning |
//! |---|---|
//! | `TAURI_DRIVER_URL` | The driver, default `http://127.0.0.1:4444` |
//! | `DESKTOP_E2E_SERVER` | The kernel's origin, the one the shell was started against |
//! | `DESKTOP_E2E_KERNEL` | The kernel's command line; `{workspace}` is replaced with a new directory |
//! | `DESKTOP_E2E_TOKEN` | The kernel's token, when it was started with one |
//! | `DESKTOP_E2E_SHELL_LOG` | The shell's log (`ADK_DESKTOP_LOG`), for its `badge.count` lines |
//! | `DESKTOP_E2E_OUT` | Where screenshots and the JSON report go |
//!
//! WebKitGTK's automation drives the first webview the shell creates, the home window; the
//! panel's steps load `/ui/tray` in it, which the home window allows.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{Value, json};

const ELEMENT: &str = "element-6066-11e4-a52e-4f735466cecf";

/// The panel's Decisions badge, or 0 without one.
const BADGE: &str = "return Number(document.querySelector('.tray-badge')?.textContent ?? 0)";

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set; see the module docs"))
}

/// The kernel's origin, ending in `/`.
fn server() -> String {
    let origin = env("DESKTOP_E2E_SERVER");
    if origin.ends_with('/') { origin } else { format!("{origin}/") }
}

fn out() -> PathBuf {
    let dir = PathBuf::from(
        std::env::var("DESKTOP_E2E_OUT").unwrap_or_else(|_| "target/desktop-e2e".into()),
    );
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A new directory under the test's output.
fn tempdir(name: &str) -> PathBuf {
    let dir = out().join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// A page bundled in the shell.
fn bundled() -> &'static str {
    if cfg!(windows) { "http://tauri.localhost/" } else { "tauri://localhost/" }
}

/// A kernel the test started; killed when dropped.
struct Kernel {
    child: tokio::process::Child,
    workspace: PathBuf,
}

impl Kernel {
    /// Starts `DESKTOP_E2E_KERNEL` in `workspace` and waits until its page answers.
    async fn start(workspace: &Path) -> Self {
        std::fs::create_dir_all(workspace).unwrap();
        let command =
            env("DESKTOP_E2E_KERNEL").replace("{workspace}", &workspace.display().to_string());
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(out().join("kernel.log"))
            .unwrap();
        let child = tokio::process::Command::new("bash")
            .arg("-c")
            .arg(format!("exec {command}"))
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let kernel = Self { child, workspace: workspace.to_path_buf() };
        let until = Instant::now() + Duration::from_secs(120);
        while http().get(format!("{}ui/", server())).send().await.is_err() {
            assert!(Instant::now() < until, "the kernel did not answer in 120 s; see kernel.log");
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        kernel
    }

    async fn stop(mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }
}

fn http() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(30)).build().unwrap()
}

/// A request to the kernel's API, with its token when it has one.
fn request(method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
    let request = http().request(method, format!("{}api{path}", server()));
    match std::env::var("DESKTOP_E2E_TOKEN") {
        Ok(token) if !token.is_empty() => request.bearer_auth(token),
        _ => request,
    }
}

async fn stopped() -> bool {
    let Ok(response) = request(reqwest::Method::GET, "/surface/status?surface=tray").send().await
    else {
        return false;
    };
    response.json::<Value>().await.is_ok_and(|status| status["stopped"] == json!(true))
}

/// Sends `text` to a new conversation as the web home does.
async fn send(session: &str, text: &str) {
    let body = json!({
        "surface": "web",
        "origin": { "kind": "keyboard" },
        "command": { "type": "send", "sessionId": session, "text": text }
    });
    let response =
        request(reqwest::Method::POST, "/surface/commands").json(&body).send().await.unwrap();
    let status = response.status();
    assert!(status.is_success(), "send: {status} {}", response.text().await.unwrap_or_default());
}

/// Waits until the shell's log has a `badge.count=<count>` line after line `after`, and returns
/// its line number.
async fn badge_logged(count: u32, after: usize, within: Duration) -> usize {
    let path = env("DESKTOP_E2E_SHELL_LOG");
    let needle = format!("badge.count={count}");
    let until = Instant::now() + within;
    loop {
        let log = std::fs::read_to_string(&path).unwrap_or_default();
        if let Some((line, _)) =
            log.lines().enumerate().skip(after).find(|(_, line)| line.contains(&needle))
        {
            return line + 1;
        }
        assert!(Instant::now() < until, "no `{needle}` in {path} within {within:?}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Clicks Stop all and returns how long the kernel took to report itself stopped.
async fn stop_all(driver: &Driver) -> Duration {
    let stop = driver.wait("//button[normalize-space()='Stop all']", Duration::from_secs(5)).await;
    let clicked = Instant::now();
    driver.click(&stop).await;
    while !stopped().await {
        assert!(clicked.elapsed() < Duration::from_millis(1_500), "not stopped within 1.5 s");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    clicked.elapsed()
}

fn report(name: &str, value: &Value) {
    let text = serde_json::to_string_pretty(value).unwrap();
    std::fs::write(out().join(format!("{name}.json")), &text).unwrap();
    println!("{text}");
}

/// A W3C WebDriver session over plain HTTP.
struct Driver {
    http: reqwest::Client,
    base: String,
}

impl Driver {
    async fn start() -> Self {
        let url =
            std::env::var("TAURI_DRIVER_URL").unwrap_or_else(|_| "http://127.0.0.1:4444".into());
        let http = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(120))
            .build()
            .unwrap();
        let application = std::env::var("ADK_DESKTOP_BIN")
            .unwrap_or_else(|_| env!("CARGO_BIN_EXE_adk-desktop").to_owned());
        let capabilities = json!({
            "capabilities": {
                "alwaysMatch": { "browserName": "wry", "tauri:options": { "application": application } }
            }
        });
        let started: Value = http
            .post(format!("{url}/session"))
            .json(&capabilities)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let session = started["value"]["sessionId"]
            .as_str()
            .unwrap_or_else(|| panic!("no session: {started}"))
            .to_owned();
        Self { http, base: format!("{url}/session/{session}") }
    }

    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, String> {
        let request = self.http.request(method, format!("{}{path}", self.base));
        let request = match body {
            Some(body) => request.json(&body),
            None => request,
        };
        let response = request.send().await.map_err(|error| error.to_string())?;
        let ok = response.status().is_success();
        let value: Value = response.json().await.map_err(|error| error.to_string())?;
        if ok { Ok(value["value"].clone()) } else { Err(value["value"].to_string()) }
    }

    async fn goto(&self, url: &str) {
        self.call(reqwest::Method::POST, "/url", Some(json!({ "url": url }))).await.unwrap();
    }

    async fn url(&self) -> String {
        let url = self.call(reqwest::Method::GET, "/url", None).await.unwrap_or_default();
        url.as_str().unwrap_or_default().to_owned()
    }

    async fn find(&self, xpath: &str) -> Option<String> {
        let body = json!({ "using": "xpath", "value": xpath });
        let found = self.call(reqwest::Method::POST, "/element", Some(body)).await.ok()?;
        found[ELEMENT].as_str().map(str::to_owned)
    }

    /// Waits until `xpath` finds an element.
    async fn wait(&self, xpath: &str, within: Duration) -> String {
        let until = Instant::now() + within;
        loop {
            if let Some(element) = self.find(xpath).await {
                return element;
            }
            assert!(
                Instant::now() < until,
                "{xpath} did not appear within {within:?}; at {}",
                self.url().await
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// Waits until `script` returns something truthy, and returns it.
    async fn wait_script(&self, script: &str, within: Duration) -> Value {
        let until = Instant::now() + within;
        loop {
            if let Ok(value) = self.execute(script).await
                && !value.is_null()
                && value != json!(false)
            {
                return value;
            }
            assert!(Instant::now() < until, "`{script}` stayed false for {within:?}");
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    async fn execute(&self, script: &str) -> Result<Value, String> {
        let body = json!({ "script": script, "args": [] });
        self.call(reqwest::Method::POST, "/execute/sync", Some(body)).await
    }

    async fn click(&self, element: &str) {
        let path = format!("/element/{element}/click");
        self.call(reqwest::Method::POST, &path, Some(json!({}))).await.unwrap();
    }

    async fn type_into(&self, element: &str, text: &str) {
        let path = format!("/element/{element}/value");
        self.call(reqwest::Method::POST, &path, Some(json!({ "text": text }))).await.unwrap();
    }

    async fn resize(&self, width: u32, height: u32) {
        let body = json!({ "width": width, "height": height });
        let _ = self.call(reqwest::Method::POST, "/window/rect", Some(body)).await;
    }

    async fn screenshot(&self, name: &str) {
        let Ok(value) = self.call(reqwest::Method::GET, "/screenshot", None).await else { return };
        let png = value
            .as_str()
            .and_then(|text| base64::engine::general_purpose::STANDARD.decode(text).ok());
        if let Some(png) = png {
            std::fs::write(out().join(format!("{name}.png")), png).unwrap();
        }
    }

    /// Screenshots the page in light and in dark, as the home's theme switch sets them.
    async fn screenshots(&self, name: &str) {
        for theme in ["light", "dark"] {
            let _ = self
                .execute(&format!(
                    "try {{ localStorage.setItem('adk.home.theme', '{theme}'); }} catch (e) {{}} \
                     document.documentElement.dataset.theme = '{theme}'; return true;"
                ))
                .await;
            tokio::time::sleep(Duration::from_millis(400)).await;
            self.screenshot(&format!("{name}-{theme}")).await;
        }
        let _ = self
            .execute(
                "localStorage.removeItem('adk.home.theme'); \
                 delete document.documentElement.dataset.theme; return true;",
            )
            .await;
    }

    async fn quit(self) {
        let _ = self.http.delete(&self.base).send().await;
    }
}

/// Step 1 of the WebKitGTK spike (go or no-go): the home renders and a held call is allowed
/// once from it; the probe's terminal echoes on the DOM renderer, and its 3,000-line diff
/// scrolls and selects. WebGL may fail; the report says so.
#[tokio::test]
#[ignore = "needs Linux, tauri-driver, WebKitWebDriver, xvfb and scripted_home; run by desktop.yml"]
async fn spike_home_and_probe() {
    let kernel = Kernel::start(&tempdir("spike")).await;
    let driver = Driver::start().await;
    let mut result = json!({ "server": server() });

    // The home renders.
    driver.wait("//button[normalize-space()='Stop all']", Duration::from_secs(60)).await;
    result["home"] = json!("rendered");
    driver.screenshots("spike-home").await;

    // A held call, allowed once from the home, runs once.
    let message =
        driver.wait("//input[@placeholder='Message your agents']", Duration::from_secs(10)).await;
    driver.type_into(&message, "Write the October invoice\u{E007}").await;
    let allow = driver
        .wait(
            "//main//article[.//h3[normalize-space()='Write file invoice.txt']]\
             //button[normalize-space()='Allow once']",
            Duration::from_secs(30),
        )
        .await;
    assert!(!kernel.workspace.join("invoice.txt").exists(), "nothing runs before the answer");
    driver.screenshots("spike-decision").await;
    driver.click(&allow).await;
    driver
        .wait(
            "//main//*[contains(normalize-space(), 'The invoice is written.')]",
            Duration::from_secs(30),
        )
        .await;
    assert_eq!(std::fs::read_to_string(kernel.workspace.join("invoice.txt")).unwrap(), "INV-1");
    result["decide"] = json!("allowed once; invoice.txt written once");

    // The probe, bundled in the shell.
    driver.goto(&format!("{}probe.html", bundled())).await;
    driver
        .wait_script(
            "return window.__probe && window.__probe.done ? true : false;",
            Duration::from_secs(60),
        )
        .await;
    let terminal = driver.wait("//*[@id='term-dom']//textarea", Duration::from_secs(10)).await;
    driver.type_into(&terminal, "echo hi").await;
    let echoed = driver
        .wait_script(
            "return window.__probe.echo.includes('echo hi') \
             && document.querySelector('#term-dom .xterm-rows').textContent.includes('echo hi');",
            Duration::from_secs(10),
        )
        .await;
    let probe = driver.execute("return window.__probe;").await.unwrap();
    driver.screenshot("spike-probe").await;
    result["probe"] = probe.clone();
    result["echo"] = echoed;
    report("spike", &result);

    assert_eq!(probe["rendered"]["dom"], json!(true), "the DOM renderer draws");
    assert!(probe["diff"]["lines"].as_u64().unwrap_or(0) >= 3_000, "the diff has 3,000 lines");
    assert_eq!(probe["diff"]["scrolled"], json!(true), "the diff scrolls");
    assert!(probe["diff"]["selected"].as_u64().unwrap_or(0) > 0, "the diff selects");
    assert_eq!(probe["errors"], json!([]), "the probe ran without errors");
    driver.quit().await;
    kernel.stop().await;
}

/// The tray end to end in the shell (spec 3.3 §12 phase 5, the tray half): the badge follows
/// the kernel within one event, yes once from the panel, the home at a high-risk decision, Stop
/// all within about a second, and "Can't reach your agents" while the kernel is away.
#[tokio::test]
#[ignore = "needs Linux, tauri-driver, WebKitWebDriver, xvfb and scripted_tray; run by desktop.yml"]
async fn tray_end_to_end() {
    let server = server();
    let kernel = Kernel::start(&tempdir("tray")).await;
    let driver = Driver::start().await;
    let mut result = json!({ "server": server });
    driver.wait("//button[normalize-space()='Stop all']", Duration::from_secs(60)).await;

    // a. Code's held release push is there; Finance gets the October invoice.
    send("finance-october", "Send the October invoice").await;
    let two = badge_logged(2, 0, Duration::from_secs(30)).await;
    driver.goto(&format!("{server}ui/tray")).await;
    driver.wait_script(&format!("{BADGE} === 2;"), Duration::from_secs(30)).await;
    driver.resize(420, 700).await;
    driver.screenshots("tray-panel").await;
    driver.resize(1280, 860).await;

    // b. Yes once or no on Finance's card; Code's is approved only in the home.
    let finance = "//article[.//code[contains(., 'write_file')]]";
    let code = "//article[.//code[contains(., 'publish_release')]]";
    let finance_allow = format!("{finance}//button[normalize-space()='Allow once']");
    driver.wait(&finance_allow, Duration::from_secs(10)).await;
    driver
        .wait(&format!("{finance}//button[normalize-space()='Deny']"), Duration::from_secs(5))
        .await;
    let code_allow = format!("{code}//button[normalize-space()='Allow once']");
    assert!(driver.find(&code_allow).await.is_none(), "high risk is never approved on the tray");
    let open_to_decide =
        format!("{code}//*[normalize-space()='Code · High risk · open to decide']");
    driver.wait(&open_to_decide, Duration::from_secs(5)).await;
    let open = driver.wait(&format!("{code}//h3/a"), Duration::from_secs(5)).await;
    driver.click(&open).await;
    driver
        .wait(
            "//button[normalize-space()=\"Confirm it's you and approve\"]",
            Duration::from_secs(30),
        )
        .await;
    result["highRisk"] = json!("opened the home at the decision");

    // e. Allow once from the panel: written once, the badge shows 1, recorded from the tray.
    driver.goto(&format!("{server}ui/tray")).await;
    let allow = driver.wait(&finance_allow, Duration::from_secs(30)).await;
    assert!(!kernel.workspace.join("invoice.txt").exists(), "nothing runs before the answer");
    driver.click(&allow).await;
    driver
        .wait(
            "//*[starts-with(normalize-space(), 'You said yes from the tray at')]",
            Duration::from_secs(20),
        )
        .await;
    badge_logged(1, two, Duration::from_secs(30)).await;
    let written = Instant::now() + Duration::from_secs(20);
    while !kernel.workspace.join("invoice.txt").exists() {
        assert!(Instant::now() < written, "invoice.txt was not written");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(std::fs::read_to_string(kernel.workspace.join("invoice.txt")).unwrap(), "INV-1");
    result["allowOnce"] = json!("written once; badge.count=2, then badge.count=1");

    // f. Stop all from the panel: stopped within about a second, and no Resume all.
    result["stopAllMs"] = json!(stop_all(&driver).await.as_millis());
    assert!(driver.find("//button[normalize-space()='Resume all']").await.is_none());

    // g. The kernel goes away: the windows say so; it comes back: so does the shell.
    let workspace = kernel.workspace.clone();
    kernel.stop().await;
    driver
        .wait("//h1[normalize-space()=\"Can't reach your agents\"]", Duration::from_secs(30))
        .await;
    driver.screenshots("tray-away").await;
    let kernel = Kernel::start(&workspace.with_extension("again")).await;
    driver.wait("//button[normalize-space()='Stop all']", Duration::from_secs(60)).await;
    result["recovered"] = json!(true);

    report("tray", &result);
    driver.quit().await;
    kernel.stop().await;
}

/// The §12 store gate for the tray: the steps that need one agent, against `adk-rust up` on its
/// store, with the shell started with the kernel's token.
#[tokio::test]
#[ignore = "needs Linux, tauri-driver, WebKitWebDriver, xvfb and adk-rust; run by desktop.yml"]
async fn store_gate() {
    let server = server();
    let kernel = Kernel::start(&tempdir("store")).await;
    let driver = Driver::start().await;
    let store = std::env::var("DESKTOP_E2E_STORE").unwrap_or_default();
    let mut result = json!({ "server": server, "store": store });
    // The token reached the home: it reads without asking for it.
    driver.wait("//button[normalize-space()='Stop all']", Duration::from_secs(60)).await;
    let asks = "//*[normalize-space()='This page needs you signed in to the server it came from.']";
    assert!(driver.find(asks).await.is_none(), "the home asked for the token");

    send("store-gate", "Write the October invoice").await;
    let one = badge_logged(1, 0, Duration::from_secs(60)).await;
    driver.goto(&format!("{server}ui/tray")).await;
    driver.wait_script(&format!("{BADGE} === 1;"), Duration::from_secs(30)).await;
    let allow = driver
        .wait("//article//button[normalize-space()='Allow once']", Duration::from_secs(30))
        .await;
    driver.click(&allow).await;
    driver
        .wait(
            "//*[starts-with(normalize-space(), 'You said yes from the tray at')]",
            Duration::from_secs(20),
        )
        .await;
    badge_logged(0, one, Duration::from_secs(30)).await;
    result["allowOnce"] = json!("from the tray; badge.count=1, then badge.count=0");
    result["stopAllMs"] = json!(stop_all(&driver).await.as_millis());

    let workspace = kernel.workspace.clone();
    kernel.stop().await;
    driver
        .wait("//h1[normalize-space()=\"Can't reach your agents\"]", Duration::from_secs(30))
        .await;
    let kernel = Kernel::start(&workspace).await;
    driver
        .wait(
            "//button[normalize-space()='Stop all' or normalize-space()='Resume all']",
            Duration::from_secs(60),
        )
        .await;
    result["recovered"] = json!(true);

    report("store-gate", &result);
    driver.quit().await;
    kernel.stop().await;
}
