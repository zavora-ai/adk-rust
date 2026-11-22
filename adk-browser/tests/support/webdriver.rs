//! A recorded WebDriver server for tests: it answers the W3C WebDriver commands the browser
//! tools send with recorded pages instead of running a browser.
//!
//! The site has three pages — `https://example.com/` with a heading, a link, a search box
//! and a button; the page that link opens; and a search results page — and the fixture
//! follows clicks, typing and Enter between them. Scripts are recognised by the
//! `/*adk-browser:<name>*/` marker the tools put on each one. Every command is logged, so a
//! test can assert what reached the browser.

#![allow(dead_code)]

use base64::Engine;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

pub const HOME: &str = "https://example.com/";
pub const MORE: &str = "https://www.iana.org/help/example-domains";

#[derive(Clone)]
struct Element {
    reference: &'static str,
    role: &'static str,
    name: &'static str,
    center: (i64, i64),
    href: Option<&'static str>,
}

fn elements(url: &str) -> Vec<Element> {
    if url.starts_with(HOME) && !url.contains("search") {
        vec![
            Element {
                reference: "ref_1",
                role: "heading",
                name: "Example Domain",
                center: (640, 120),
                href: None,
            },
            Element {
                reference: "ref_2",
                role: "link",
                name: "More information...",
                center: (640, 300),
                href: Some(MORE),
            },
            Element {
                reference: "ref_3",
                role: "textbox",
                name: "Search",
                center: (560, 420),
                href: None,
            },
            Element {
                reference: "ref_4",
                role: "button",
                name: "Search",
                center: (760, 420),
                href: None,
            },
        ]
    } else if url.starts_with(MORE) {
        vec![Element {
            reference: "ref_1",
            role: "heading",
            name: "Example Domains",
            center: (640, 120),
            href: None,
        }]
    } else if url.contains("search") {
        vec![Element {
            reference: "ref_1",
            role: "heading",
            name: "Search results",
            center: (640, 120),
            href: None,
        }]
    } else {
        Vec::new()
    }
}

fn title(url: &str) -> String {
    if url.starts_with(MORE) {
        "Example Domains".into()
    } else if url.contains("search") {
        "Search results".into()
    } else if url.starts_with(HOME) {
        "Example Domain".into()
    } else {
        String::new()
    }
}

fn page_text(url: &str, query: &str) -> String {
    if url.starts_with(MORE) {
        "Example Domains\nAs described in RFC 2606 and RFC 6761, a number of domains such as \
         example.com and example.org are maintained for documentation purposes."
            .into()
    } else if url.contains("search") {
        format!("Search results\nNo results for \"{query}\" on this documentation domain.")
    } else if url.starts_with(HOME) {
        "Example Domain\nThis domain is for use in illustrative examples in documents.\nMore \
         information..."
            .into()
    } else {
        String::new()
    }
}

#[derive(Default)]
struct Tab {
    handle: String,
    history: Vec<String>,
    position: usize,
}

impl Tab {
    fn url(&self) -> String {
        self.history.get(self.position).cloned().unwrap_or_else(|| "about:blank".into())
    }

    fn visit(&mut self, url: &str) {
        self.history.truncate(self.position + 1);
        if self.history.is_empty() {
            self.history.push(url.to_string());
            self.position = 0;
        } else {
            self.history.push(url.to_string());
            self.position = self.history.len() - 1;
        }
    }
}

/// The fixture's state, readable by tests.
#[derive(Default)]
pub struct Browser {
    tabs: Vec<Tab>,
    active: usize,
    next_handle: usize,
    pointer: (i64, i64),
    focused: Option<&'static str>,
    pub typed: String,
    query: String,
    /// Every command, as `METHOD path` plus a short summary of the body.
    pub log: Vec<String>,
    /// Pointer clicks, as the element reference hit or the coordinate.
    pub clicks: Vec<String>,
    pub sessions: usize,
}

impl Browser {
    fn tab(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }

    fn url(&self) -> String {
        self.tabs.get(self.active).map(Tab::url).unwrap_or_else(|| "about:blank".into())
    }

    fn new_tab(&mut self) -> String {
        self.next_handle += 1;
        let handle = format!("window-{}", self.next_handle);
        self.tabs.push(Tab {
            handle: handle.clone(),
            history: vec!["about:blank".into()],
            position: 0,
        });
        handle
    }

    fn hit(&self, x: i64, y: i64) -> Option<Element> {
        elements(&self.url()).into_iter().find(|element| {
            (element.center.0 - x).abs() <= 80 && (element.center.1 - y).abs() <= 20
        })
    }

    fn click(&mut self, x: i64, y: i64) {
        match self.hit(x, y) {
            Some(element) => {
                self.clicks.push(element.reference.to_string());
                match (element.role, element.href) {
                    (_, Some(href)) => {
                        self.tab().visit(href);
                        self.focused = None;
                    }
                    ("textbox", _) => self.focused = Some(element.reference),
                    ("button", _) => self.search(),
                    _ => {}
                }
            }
            None => self.clicks.push(format!("({x}, {y})")),
        }
    }

    fn search(&mut self) {
        self.query = self.typed.clone();
        let url = format!("{HOME}search?q={}", self.query.replace(' ', "+"));
        self.tab().visit(&url);
        self.focused = None;
    }

    fn key(&mut self, value: &str) {
        for key in value.chars() {
            match key {
                '\u{E007}' | '\u{E006}' => self.search(),
                '\u{E003}' => {
                    self.typed.pop();
                }
                key if ('\u{E000}'..='\u{F8FF}').contains(&key) => {}
                key if self.focused.is_some() => self.typed.push(key),
                _ => {}
            }
        }
    }

    fn actions(&mut self, body: &Value) {
        for source in body["actions"].as_array().into_iter().flatten() {
            for action in source["actions"].as_array().into_iter().flatten() {
                match action["type"].as_str() {
                    Some("pointerMove") => {
                        self.pointer =
                            (action["x"].as_i64().unwrap_or(0), action["y"].as_i64().unwrap_or(0));
                    }
                    Some("pointerUp") if action["button"].as_i64() == Some(0) => {
                        let (x, y) = self.pointer;
                        self.click(x, y);
                    }
                    Some("keyDown") => {
                        let value = action["value"].as_str().unwrap_or_default().to_string();
                        self.key(&value);
                    }
                    _ => {}
                }
            }
        }
    }

    fn script(&mut self, body: &Value) -> Value {
        let script = body["script"].as_str().unwrap_or_default();
        let args = body["args"].as_array().cloned().unwrap_or_default();
        let name = script
            .strip_prefix("/*adk-browser:")
            .and_then(|rest| rest.split_once("*/"))
            .map(|(name, _)| name.to_string())
            .unwrap_or_default();
        let url = self.url();
        match name.as_str() {
            "viewport" => json!([1, 1280, 800]),
            "elements" => {
                let elements: Vec<Value> = elements(&url)
                    .into_iter()
                    .filter(|element| element.role != "heading")
                    .enumerate()
                    .map(|(i, element)| {
                        json!({
                            "index": i + 1,
                            "selector": format!("[data-ref=\"{}\"]", element.reference),
                            "role": element.role,
                            "name": element.name,
                            "tag": if element.role == "link" { "a" } else { "input" },
                            "rect": {
                                "x": element.center.0 - 80, "y": element.center.1 - 20,
                                "width": 160, "height": 40
                            }
                        })
                    })
                    .collect();
                json!({
                    "url": url,
                    "devicePixelRatio": 1,
                    "viewport": { "width": 1280, "height": 800 },
                    "elements": elements,
                    "truncated": false
                })
            }
            "read_page" => {
                let interactive_only = args.first().and_then(Value::as_str) == Some("interactive");
                let query = args.get(3).and_then(Value::as_str).map(str::to_lowercase);
                let lines: Vec<String> = elements(&url)
                    .into_iter()
                    .filter(|element| !interactive_only || element.role != "heading")
                    .filter(|element| {
                        query.as_ref().is_none_or(|query| {
                            query.split_whitespace().any(|word| {
                                format!("{} {}", element.role, element.name)
                                    .to_lowercase()
                                    .contains(word)
                            })
                        })
                    })
                    .map(|element| {
                        let href = element
                            .href
                            .map(|href| format!(" href=\"{href}\""))
                            .unwrap_or_default();
                        format!(
                            "{} \"{}\"{href} [{}]",
                            element.role, element.name, element.reference
                        )
                    })
                    .collect();
                json!({ "text": lines.join("\n"), "truncated": false, "count": lines.len() })
            }
            "get_page_text" => json!(page_text(&url, &self.query)),
            "resolve_ref" => {
                let reference = args.first().and_then(Value::as_str).unwrap_or_default();
                match elements(&url).into_iter().find(|element| element.reference == reference) {
                    Some(element) => json!([element.center.0, element.center.1]),
                    None => Value::Null,
                }
            }
            "form_input" => {
                let reference = args.first().and_then(Value::as_str).unwrap_or_default();
                match elements(&url).into_iter().find(|element| element.reference == reference) {
                    Some(element) if element.role == "textbox" => {
                        self.typed =
                            args.get(1).and_then(Value::as_str).unwrap_or_default().to_string();
                        json!({ "ok": format!("Set {reference} to \"{}\".", self.typed) })
                    }
                    Some(_) => json!({ "error": format!("{reference} is not a form element") }),
                    None => json!({ "stale": true }),
                }
            }
            "scroll" | "scroll_to" | "middle_click" => json!(true),
            "javascript_exec" => json!("fixture"),
            _ => Value::Null,
        }
    }

    fn screenshot(&self) -> String {
        let mut image = image::RgbaImage::from_pixel(1280, 800, image::Rgba([255, 255, 255, 255]));
        for element in elements(&self.url()) {
            let color = match element.role {
                "link" => image::Rgba([30, 80, 200, 255]),
                "button" => image::Rgba([40, 160, 70, 255]),
                "textbox" => image::Rgba([200, 200, 200, 255]),
                _ => image::Rgba([20, 20, 20, 255]),
            };
            for y in (element.center.1 - 12).max(0)..(element.center.1 + 12).min(800) {
                for x in (element.center.0 - 70).max(0)..(element.center.0 + 70).min(1280) {
                    image.put_pixel(x as u32, y as u32, color);
                }
            }
        }
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        base64::engine::general_purpose::STANDARD.encode(png)
    }

    fn handle(&mut self, method: &str, path: &str, body: &Value) -> Value {
        let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        let command = parts.get(2..).map(|rest| rest.join("/")).unwrap_or_default();
        let summary = match body {
            Value::Null => String::new(),
            Value::Object(map) if map.contains_key("script") => {
                let script = map["script"].as_str().unwrap_or_default();
                format!(" {}", script.lines().next().unwrap_or_default())
            }
            other => format!(" {}", other.to_string().chars().take(160).collect::<String>()),
        };
        self.log.push(format!("{method} /{command}{summary}"));
        match (method, command.as_str()) {
            ("POST", "") if parts.len() == 1 => {
                self.sessions += 1;
                if self.tabs.is_empty() {
                    self.new_tab();
                }
                json!({ "sessionId": "session-1", "capabilities": { "browserName": "chrome" } })
            }
            ("DELETE", "") => Value::Null,
            ("GET", "title") => json!(title(&self.url())),
            ("GET", "url") => json!(self.url()),
            ("POST", "url") => {
                // As a browser does: `https://example.com` loads `https://example.com/`.
                let url = body["url"].as_str().unwrap_or("about:blank");
                let url =
                    url::Url::parse(url).map(|url| url.to_string()).unwrap_or(url.to_string());
                self.tab().visit(&url);
                Value::Null
            }
            ("POST", "back") => {
                let tab = self.tab();
                tab.position = tab.position.saturating_sub(1);
                Value::Null
            }
            ("POST", "forward") => {
                let tab = self.tab();
                tab.position = (tab.position + 1).min(tab.history.len().saturating_sub(1));
                Value::Null
            }
            ("GET", "window") => json!(self.tabs[self.active].handle),
            ("GET", "window/handles") => {
                json!(self.tabs.iter().map(|tab| tab.handle.clone()).collect::<Vec<_>>())
            }
            ("POST", "window/new") => {
                let handle = self.new_tab();
                json!({ "handle": handle, "type": "tab" })
            }
            ("POST", "window") => {
                let handle = body["handle"].as_str().unwrap_or_default();
                if let Some(index) = self.tabs.iter().position(|tab| tab.handle == handle) {
                    self.active = index;
                }
                Value::Null
            }
            ("DELETE", "window") => {
                if !self.tabs.is_empty() {
                    self.tabs.remove(self.active);
                    self.active = 0;
                }
                json!(self.tabs.iter().map(|tab| tab.handle.clone()).collect::<Vec<_>>())
            }
            ("GET", "screenshot") => json!(self.screenshot()),
            ("POST", "execute/sync") => self.script(body),
            ("POST", "actions") => {
                self.actions(body);
                Value::Null
            }
            ("POST", "element") => json!({ "element-6066-11e4-a52e-4f735466cecf": "element-1" }),
            ("POST", value) if value.ends_with("/value") => {
                self.typed.push_str(body["text"].as_str().unwrap_or_default());
                Value::Null
            }
            ("POST", "window/rect") | ("GET", "window/rect") => {
                json!({ "x": 0, "y": 0, "width": 1280, "height": 800 })
            }
            _ => Value::Null,
        }
    }
}

/// A running recorded WebDriver server.
pub struct WebDriverFixture {
    pub address: SocketAddr,
    pub browser: Arc<Mutex<Browser>>,
    stopped: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl WebDriverFixture {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let browser = Arc::new(Mutex::new(Browser::default()));
        let stopped = Arc::new(AtomicBool::new(false));
        let worker = {
            let browser = browser.clone();
            let stopped = stopped.clone();
            std::thread::spawn(move || {
                for connection in listener.incoming() {
                    if stopped.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(mut connection) = connection else { continue };
                    let _ = serve(&mut connection, &browser);
                }
            })
        };
        Self { address, browser, stopped, worker: Some(worker) }
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.address)
    }

    pub fn log(&self) -> Vec<String> {
        self.browser.lock().unwrap().log.clone()
    }

    pub fn clicks(&self) -> Vec<String> {
        self.browser.lock().unwrap().clicks.clone()
    }

    pub fn current_url(&self) -> String {
        self.browser.lock().unwrap().url()
    }
}

fn serve(connection: &mut TcpStream, browser: &Arc<Mutex<Browser>>) -> std::io::Result<()> {
    connection.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
    let mut reader = BufReader::new(connection.try_clone()?);
    let mut request = String::new();
    reader.read_line(&mut request)?;
    let mut length = 0;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header)?;
        if header == "\r\n" || header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse::<usize>().unwrap_or(0);
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let mut words = request.split_whitespace();
    let method = words.next().unwrap_or_default().to_string();
    let path = words.next().unwrap_or_default().to_string();
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let value = browser.lock().unwrap().handle(&method, &path, &body);
    let body = json!({ "value": value }).to_string();
    write!(
        connection,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    connection.flush()
}

impl Drop for WebDriverFixture {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
