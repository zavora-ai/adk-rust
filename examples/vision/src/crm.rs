//! S14's web side: a small CRM served in process over HTTP, and a stand-in WebDriver server
//! the browser tools drive.
//!
//! The CRM's pages place every element absolutely, in CSS pixels of a 1280×800 viewport, and
//! carry the same layout as JSON in an `adk-layout` block. A browser computes each element's
//! rectangle from the page's styles; the stand-in WebDriver lays the page out from that block,
//! answers the W3C WebDriver commands `adk-browser` sends (and the scripts it marks
//! `/*adk-browser:<name>*/`), and renders screenshots at device pixel ratio 2. It loads every
//! page from the CRM over HTTP and logs each command, so the scenario can check what reached
//! the browser.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};

use adk_vision::DecodedImage;
use anyhow::Result;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

/// The viewport's size in CSS pixels, and its device pixel ratio.
pub const VIEWPORT: (u32, u32) = (1280, 800);
pub const RATIO: u32 = 2;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// One customer in the CRM.
#[derive(Clone, Debug, PartialEq)]
pub struct Customer {
    pub id: String,
    /// The record's fields, in the order the CRM shows them.
    pub fields: Vec<(String, String)>,
}

impl Customer {
    pub fn get(&self, label: &str) -> Option<&str> {
        self.fields.iter().find(|(l, _)| l.eq_ignore_ascii_case(label)).map(|(_, v)| v.as_str())
    }
}

/// An element as the CRM lays it out: its rectangle in CSS pixels and the selectors that
/// match it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Element {
    pub selector: String,
    #[serde(default)]
    pub matches: Vec<String>,
    pub role: String,
    pub name: String,
    pub tag: String,
    pub text: String,
    /// `[x, y, width, height]` in CSS pixels.
    pub rect: [f32; 4],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
    pub interactive: bool,
}

impl Element {
    pub fn contains(&self, point: [f32; 2]) -> bool {
        let [x, y, w, h] = self.rect;
        point[0] >= x && point[0] <= x + w && point[1] >= y && point[1] <= y + h
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Page {
    title: String,
    elements: Vec<Element>,
}

fn element(selector: &str, role: &str, tag: &str, text: &str, rect: [f32; 4]) -> Element {
    Element {
        selector: selector.into(),
        matches: Vec::new(),
        role: role.into(),
        name: text.into(),
        tag: tag.into(),
        text: text.into(),
        rect,
        href: None,
        interactive: matches!(role, "link" | "button" | "textbox"),
    }
}

fn list_page(customers: &[Customer]) -> Page {
    let mut elements = vec![
        element("#title", "heading", "h1", "Customers", [32.0, 24.0, 400.0, 40.0]),
        element("#search", "textbox", "input", "Search customers", [32.0, 88.0, 360.0, 36.0]),
        element("#new-customer", "button", "button", "New customer", [1100.0, 88.0, 148.0, 36.0]),
    ];
    for (i, customer) in customers.iter().enumerate() {
        let name = customer.get("Name").unwrap_or_default();
        let mut link = element(
            &format!("#customer-{}", customer.id),
            "link",
            "a",
            &format!("{} {name}", customer.id),
            [32.0, 160.0 + 44.0 * i as f32, 420.0, 32.0],
        );
        link.href = Some(format!("/customers/{}", customer.id));
        elements.push(link);
    }
    Page { title: "Customers - Larch CRM".into(), elements }
}

fn record_page(customer: &Customer) -> Page {
    let mut back = element("#back", "link", "a", "Back to customers", [32.0, 24.0, 200.0, 28.0]);
    back.href = Some("/customers".into());
    let mut elements = vec![
        back,
        element("#name", "heading", "h1", customer.get("Name").unwrap_or_default(), [
            32.0, 64.0, 600.0, 40.0,
        ]),
    ];
    for (i, (label, value)) in customer.fields.iter().enumerate() {
        let y = 128.0 + 44.0 * i as f32;
        let n = i + 1;
        let mut dt = element(&format!("#record dt:nth-of-type({n})"), "term", "dt", label, [
            32.0, y, 140.0, 28.0,
        ]);
        dt.matches = vec!["#record dt".into(), "dt".into()];
        let mut dd = element(&format!("#record dd:nth-of-type({n})"), "definition", "dd", value, [
            200.0, y, 600.0, 28.0,
        ]);
        dd.matches = vec!["#record dd".into(), "dd".into()];
        elements.extend([dt, dd]);
    }
    elements.push(element("#edit", "button", "button", "Edit", [32.0, 380.0, 100.0, 36.0]));
    Page { title: format!("{} - Larch CRM", customer.id), elements }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The page as HTML: each element absolutely placed, and the layout block.
fn html(page: &Page) -> String {
    let mut body = String::new();
    let mut in_record = false;
    for e in &page.elements {
        let record = matches!(e.tag.as_str(), "dt" | "dd");
        if record && !in_record {
            body.push_str("<dl id=\"record\">");
        } else if !record && in_record {
            body.push_str("</dl>");
        }
        in_record = record;
        let [x, y, w, h] = e.rect;
        let style = format!(
            "position:absolute;margin:0;left:{x}px;top:{y}px;width:{w}px;height:{h}px"
        );
        let id = e.selector.strip_prefix('#').filter(|s| !s.contains(' '));
        let id = id.map(|id| format!(" id=\"{id}\"")).unwrap_or_default();
        let href = e.href.as_ref().map(|h| format!(" href=\"{}\"", escape(h))).unwrap_or_default();
        let text = escape(&e.text);
        let tag = &e.tag;
        if tag == "input" {
            body.push_str(&format!(
                "<input{id} style=\"{style}\" placeholder=\"{text}\" aria-label=\"{text}\">"
            ));
        } else {
            body.push_str(&format!("<{tag}{id}{href} style=\"{style}\">{text}</{tag}>"));
        }
    }
    if in_record {
        body.push_str("</dl>");
    }
    let layout = serde_json::to_string(page).unwrap_or_default();
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{}</title></head>\
         <body style=\"margin:0;font:16px sans-serif\">{body}\
         <script type=\"application/json\" id=\"adk-layout\">{layout}</script></body></html>",
        escape(&page.title)
    )
}

/// The CRM: its customers, served over HTTP on loopback.
pub struct Crm {
    pub address: SocketAddr,
    pub customers: Vec<Customer>,
    /// Every path requested, in order.
    pub requests: Arc<Mutex<Vec<String>>>,
}

impl Crm {
    pub async fn serve(customers: Vec<Customer>) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let requests = Arc::new(Mutex::new(Vec::new()));
        let pages: BTreeMap<String, String> = std::iter::once((
            "/customers".to_string(),
            html(&list_page(&customers)),
        ))
        .chain(customers.iter().map(|c| (format!("/customers/{}", c.id), html(&record_page(c)))))
        .collect();
        let log = requests.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let pages = pages.clone();
                let log = log.clone();
                tokio::spawn(async move {
                    let Ok((method, path, _)) = read_request(&mut stream).await else { return };
                    lock(&log).push(path.clone());
                    let (status, body) = match (method.as_str(), pages.get(&path)) {
                        ("GET", Some(page)) => ("200 OK", page.clone()),
                        _ => ("404 Not Found", "<!doctype html><title>Not found</title>".into()),
                    };
                    let _ = respond(&mut stream, status, "text/html; charset=utf-8", &body).await;
                });
            }
        });
        Ok(Self { address, customers, requests })
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }

    pub fn customer(&self, id: &str) -> Option<&Customer> {
        self.customers.iter().find(|c| c.id == id)
    }
}

async fn read_request(stream: &mut TcpStream) -> Result<(String, String, Value)> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let mut words = line.split_whitespace();
    let method = words.next().unwrap_or_default().to_string();
    let path = words.next().unwrap_or_default().to_string();
    let mut length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).await? == 0 || header == "\r\n" || header == "\n" {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).await?;
    Ok((method, path, serde_json::from_slice(&body).unwrap_or(Value::Null)))
}

async fn respond(stream: &mut TcpStream, status: &str, kind: &str, body: &str) -> Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

/// A click that reached the browser: the element's selector and rectangle.
#[derive(Clone, Debug)]
pub struct Click {
    pub selector: String,
    pub rect: [f32; 4],
    pub url: String,
}

#[derive(Default)]
struct BrowserState {
    url: String,
    page: Option<Page>,
    generation: usize,
    /// Every command, as `METHOD command`.
    log: Vec<String>,
    /// Every page loaded, by URL.
    loads: Vec<String>,
    clicks: Vec<Click>,
}

/// The stand-in WebDriver server.
pub struct StandInBrowser {
    pub address: SocketAddr,
    state: Arc<Mutex<BrowserState>>,
}

impl StandInBrowser {
    pub async fn start() -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let state = Arc::new(Mutex::new(BrowserState::default()));
        let shared = state.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let state = shared.clone();
                tokio::spawn(async move {
                    let Ok((method, path, body)) = read_request(&mut stream).await else { return };
                    let (status, value) = handle(&state, &method, &path, &body).await;
                    let body = json!({ "value": value }).to_string();
                    let _ = respond(&mut stream, status, "application/json", &body).await;
                });
            }
        });
        Ok(Self { address, state })
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.address)
    }

    pub fn log(&self) -> Vec<String> {
        lock(&self.state).log.clone()
    }

    pub fn loads(&self) -> Vec<String> {
        lock(&self.state).loads.clone()
    }

    pub fn clicks(&self) -> Vec<Click> {
        lock(&self.state).clicks.clone()
    }
}

fn no_such(what: &str, message: String) -> (&'static str, Value) {
    ("404 Not Found", json!({ "error": what, "message": message, "stacktrace": "" }))
}

const ELEMENT_KEY: &str = "element-6066-11e4-a52e-4f735466cecf";

async fn load(state: &Arc<Mutex<BrowserState>>, url: &str) -> (&'static str, Value) {
    let page = async {
        let html = reqwest::get(url).await?.error_for_status()?.text().await?;
        let start = html.find("id=\"adk-layout\">").map(|i| i + "id=\"adk-layout\">".len());
        let end = html.rfind("</script>");
        let block = match (start, end) {
            (Some(s), Some(e)) if s < e => &html[s..e],
            _ => anyhow::bail!("{url} has no layout block"),
        };
        Ok::<Page, anyhow::Error>(serde_json::from_str(block)?)
    }
    .await;
    match page {
        Ok(page) => {
            let mut s = lock(state);
            s.url = url.to_string();
            s.page = Some(page);
            s.generation += 1;
            s.loads.push(url.to_string());
            ("200 OK", Value::Null)
        }
        Err(error) => (
            "500 Internal Server Error",
            json!({ "error": "unknown error", "message": error.to_string(), "stacktrace": "" }),
        ),
    }
}

/// The element a WebDriver element ID names on the current page.
fn resolve(s: &BrowserState, id: &str) -> Option<Element> {
    let (generation, index) = id.strip_prefix("el-")?.split_once('-')?;
    if generation.parse::<usize>().ok()? != s.generation {
        return None;
    }
    s.page.as_ref()?.elements.get(index.parse::<usize>().ok()?).cloned()
}

fn find(s: &BrowserState, selector: &str) -> Vec<Value> {
    let Some(page) = &s.page else { return Vec::new() };
    page.elements
        .iter()
        .enumerate()
        .filter(|(_, e)| e.selector == selector || e.matches.iter().any(|m| m == selector))
        .map(|(i, _)| json!({ ELEMENT_KEY: format!("el-{}-{i}", s.generation) }))
        .collect()
}

fn screenshot(page: Option<&Page>) -> String {
    let (w, h) = (VIEWPORT.0 * RATIO, VIEWPORT.1 * RATIO);
    let mut rgba = vec![255u8; (w * h * 4) as usize];
    for e in page.map(|p| p.elements.as_slice()).unwrap_or_default() {
        let colour = match e.role.as_str() {
            "link" => [210, 222, 245],
            "button" => [214, 232, 214],
            "textbox" => [236, 236, 236],
            _ => [244, 244, 244],
        };
        let [x, y, ew, eh] = e.rect.map(|v| (v * RATIO as f32) as u32);
        for py in y..(y + eh).min(h) {
            for px in x..(x + ew).min(w) {
                let at = ((py * w + px) * 4) as usize;
                rgba[at..at + 3].copy_from_slice(&colour);
            }
        }
    }
    let png = DecodedImage::from_rgba(w, h, rgba)
        .and_then(|image| image.encode_png())
        .unwrap_or_default();
    base64::engine::general_purpose::STANDARD.encode(png)
}

async fn handle(
    state: &Arc<Mutex<BrowserState>>,
    method: &str,
    path: &str,
    body: &Value,
) -> (&'static str, Value) {
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    if parts.first() == Some(&"status") {
        return ("200 OK", json!({ "ready": true, "message": "stand-in WebDriver" }));
    }
    let command = parts.get(2..).map(|rest| rest.join("/")).unwrap_or_default();
    lock(state).log.push(format!("{method} /{command}"));
    match (method, command.as_str()) {
        ("POST", "") if parts.len() == 1 => (
            "200 OK",
            json!({ "sessionId": "s14", "capabilities": { "browserName": "stand-in" } }),
        ),
        ("POST", "url") => {
            let url = body["url"].as_str().unwrap_or("about:blank").to_string();
            load(state, &url).await
        }
        ("GET", "url") => ("200 OK", json!(lock(state).url)),
        ("GET", "title") => {
            ("200 OK", json!(lock(state).page.as_ref().map(|p| p.title.clone()).unwrap_or_default()))
        }
        ("GET", "window/rect") | ("POST", "window/rect") => (
            "200 OK",
            json!({ "x": 0, "y": 0, "width": VIEWPORT.0, "height": VIEWPORT.1 }),
        ),
        ("GET", "screenshot") => ("200 OK", json!(screenshot(lock(state).page.as_ref()))),
        ("POST", "element") => {
            let selector = body["value"].as_str().unwrap_or_default();
            match find(&lock(state), selector).into_iter().next() {
                Some(found) => ("200 OK", found),
                None => no_such("no such element", format!("no element matches {selector}")),
            }
        }
        ("POST", "elements") => {
            let selector = body["value"].as_str().unwrap_or_default();
            ("200 OK", Value::Array(find(&lock(state), selector)))
        }
        ("POST", "execute/sync") => ("200 OK", script(&lock(state), body)),
        (_, other) if other.starts_with("element/") => {
            let rest: Vec<&str> = other.split('/').collect();
            let id = rest.get(1).copied().unwrap_or_default();
            let what = rest.get(2..).map(|r| r.join("/")).unwrap_or_default();
            let Some(e) = resolve(&lock(state), id) else {
                return no_such("stale element reference", format!("{id} is not on this page"));
            };
            match (method, what.as_str()) {
                ("POST", "click") => {
                    let url = {
                        let mut s = lock(state);
                        let url = s.url.clone();
                        s.clicks.push(Click { selector: e.selector.clone(), rect: e.rect, url });
                        s.url.clone()
                    };
                    match &e.href {
                        Some(href) => {
                            let base = reqwest::Url::parse(&url).ok();
                            let next = base
                                .and_then(|b| b.join(href).ok())
                                .map(|u| u.to_string())
                                .unwrap_or_else(|| href.clone());
                            load(state, &next).await
                        }
                        None => ("200 OK", Value::Null),
                    }
                }
                ("GET", "text") => ("200 OK", json!(e.text)),
                ("GET", "name") => ("200 OK", json!(e.tag)),
                ("GET", "displayed") | ("GET", "enabled") => ("200 OK", json!(true)),
                ("GET", "selected") => ("200 OK", json!(false)),
                ("GET", "rect") => {
                    let [x, y, w, h] = e.rect;
                    ("200 OK", json!({ "x": x, "y": y, "width": w, "height": h }))
                }
                ("GET", attribute) if attribute.starts_with("attribute/") => {
                    let name = attribute.trim_start_matches("attribute/");
                    let value = match name {
                        "href" => e.href.clone().map(Value::String).unwrap_or(Value::Null),
                        "id" => json!(e.selector.trim_start_matches('#')),
                        _ => Value::Null,
                    };
                    ("200 OK", value)
                }
                _ => ("200 OK", Value::Null),
            }
        }
        _ => ("200 OK", Value::Null),
    }
}

/// The page's answer to a script `adk-browser` sends, recognised by its marker.
fn script(s: &BrowserState, body: &Value) -> Value {
    let source = body["script"].as_str().unwrap_or_default();
    let marker = source
        .strip_prefix("/*adk-browser:")
        .and_then(|rest| rest.split_once("*/"))
        .map(|(name, _)| name);
    let elements = s.page.as_ref().map(|p| p.elements.as_slice()).unwrap_or_default();
    match marker {
        Some("viewport") => json!([RATIO, VIEWPORT.0, VIEWPORT.1]),
        Some("elements") => {
            let found: Vec<Value> = elements
                .iter()
                .filter(|e| e.interactive)
                .enumerate()
                .map(|(i, e)| {
                    let [x, y, w, h] = e.rect;
                    json!({
                        "index": i + 1,
                        "selector": e.selector,
                        "role": e.role,
                        "name": e.name,
                        "tag": e.tag,
                        "rect": { "x": x, "y": y, "width": w, "height": h },
                    })
                })
                .collect();
            json!({
                "url": s.url,
                "devicePixelRatio": RATIO,
                "viewport": { "width": VIEWPORT.0, "height": VIEWPORT.1 },
                "elements": found,
                "truncated": false,
            })
        }
        _ if source.contains("innerText") => {
            let text: Vec<&str> = elements.iter().map(|e| e.text.as_str()).collect();
            json!(text.join("\n"))
        }
        _ => Value::Null,
    }
}
