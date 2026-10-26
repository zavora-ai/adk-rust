//! Host access logs as the `host_logs` KPI source (spec 3.5 §7.12): a parser for the common
//! log formats and daily counts without addresses.

use std::sync::Arc;

use adk_kernel::charter::KpiSource;
use adk_server::awp::BeaconStore;
use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use uuid::Uuid;

use super::{Tally, check_site, total};

/// AI crawlers and assistants by the token their user agent carries, as counted under
/// `agents:{name}`. A user agent is a claim: verified agents are counted at the edge.
const AGENTS: &[&str] = &[
    "gptbot",
    "chatgpt-user",
    "oai-searchbot",
    "claudebot",
    "claude-user",
    "claude-searchbot",
    "anthropic-ai",
    "perplexitybot",
    "perplexity-user",
    "googleother",
    "google-cloudvertexbot",
    "ccbot",
    "bytespider",
    "amazonbot",
    "applebot",
    "meta-externalagent",
    "meta-externalfetcher",
    "duckassistbot",
    "mistralai-user",
    "cohere-ai",
    "youbot",
];

/// Markers of automated clients that are not people.
const CRAWLERS: &[&str] = &[
    "bot",
    "crawl",
    "spider",
    "slurp",
    "facebookexternalhit",
    "headless",
    "python-requests",
    "curl/",
    "wget/",
    "go-http-client",
    "httpclient",
];

/// One request from a host log (spec 3.5 §7.12). The client's address is never read into
/// it, and the path carries no query string or fragment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogLine {
    /// When the request was served.
    pub at: DateTime<Utc>,
    /// The method.
    pub method: String,
    /// The path, without query or fragment.
    pub path: String,
    /// The status.
    pub status: u16,
    /// Bytes sent.
    pub bytes: u64,
    /// The user agent, when the format has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
}

/// Parses access logs line by line (spec 3.5 §7.12): the Common Log Format, the Combined
/// Log Format (Apache, nginx and most hosts), and the W3C extended format whose `#Fields:`
/// line names the columns (IIS, CloudFront).
///
/// # Example
///
/// ```rust
/// use adk_website::measurement::HostLogParser;
///
/// let mut parser = HostLogParser::new();
/// let line = parser
///     .parse_line(r#"198.51.100.4 - - [24/Oct/2026:09:15:02 +0300] "GET /a/?q=1 HTTP/1.1" 200 512"#)
///     .unwrap();
/// assert_eq!((line.path.as_str(), line.status, line.bytes), ("/a/", 200, 512));
/// assert_eq!(line.at.to_rfc3339(), "2026-10-24T06:15:02+00:00");
///
/// assert!(parser.parse_line("#Fields: date time c-ip cs-method cs-uri-stem sc-status sc-bytes cs(User-Agent)").is_none());
/// let line = parser.parse_line("2026-10-24 09:15:02 198.51.100.4 GET /b.html 404 0 Mozilla/5.0+(X11)").unwrap();
/// assert_eq!(line.user_agent.as_deref(), Some("Mozilla/5.0 (X11)"));
/// ```
#[derive(Debug, Clone, Default)]
pub struct HostLogParser {
    fields: Option<Vec<String>>,
}

impl HostLogParser {
    /// A parser that has seen no `#Fields:` line.
    pub fn new() -> Self {
        Self::default()
    }

    /// The request on `line`; `None` for a blank line, a directive, or a line in no format
    /// it reads. A `#Fields:` line sets the columns of the W3C lines after it.
    pub fn parse_line(&mut self, line: &str) -> Option<LogLine> {
        let line = line.trim_end_matches(['\r', '\n']);
        if let Some(fields) = line.strip_prefix("#Fields:") {
            self.fields = Some(fields.split_whitespace().map(str::to_ascii_lowercase).collect());
            return None;
        }
        if line.trim().is_empty() || line.starts_with('#') {
            return None;
        }
        if let Some(fields) = &self.fields
            && let Some(parsed) = parse_w3c(fields, line)
        {
            return Some(parsed);
        }
        parse_clf(line)
    }
}

/// A Common or Combined Log Format line: `host ident user [time] "request" status bytes`,
/// then `"referer" "user-agent"` in the combined format.
fn parse_clf(line: &str) -> Option<LogLine> {
    let open = line.find('[')?;
    let close = open + line[open..].find(']')?;
    let at = DateTime::parse_from_str(&line[open + 1..close], "%d/%b/%Y:%H:%M:%S %z")
        .ok()?
        .with_timezone(&Utc);
    let (request, rest) = quoted(&line[close + 1..])?;
    let (status, rest) = token(rest)?;
    let (bytes, rest) = token(rest)?;
    let user_agent = quoted(rest)
        .and_then(|(_, rest)| quoted(rest))
        .map(|(agent, _)| agent)
        .filter(|agent| !agent.is_empty() && agent != "-");
    let mut parts = request.split_whitespace();
    let method = parts.next()?.to_string();
    let path = clean_path(parts.next()?)?;
    Some(LogLine {
        at,
        method,
        path,
        status: status.parse().ok()?,
        bytes: bytes.parse().unwrap_or(0),
        user_agent,
    })
}

/// A W3C extended line in the columns `fields` names.
fn parse_w3c(fields: &[String], line: &str) -> Option<LogLine> {
    let values: Vec<&str> = line.split_whitespace().collect();
    if values.len() != fields.len() {
        return None;
    }
    let get = |name: &str| {
        fields
            .iter()
            .position(|field| field == name)
            .map(|index| values[index])
            .filter(|value| *value != "-")
    };
    let stamp = format!("{} {}", get("date")?, get("time")?);
    let at = NaiveDateTime::parse_from_str(&stamp, "%Y-%m-%d %H:%M:%S").ok()?.and_utc();
    Some(LogLine {
        at,
        method: get("cs-method")?.to_string(),
        path: clean_path(&percent_decode(get("cs-uri-stem")?, false))?,
        status: get("sc-status")?.parse().ok()?,
        bytes: get("sc-bytes").and_then(|bytes| bytes.parse().ok()).unwrap_or(0),
        user_agent: get("cs(user-agent)").map(|agent| percent_decode(agent, true)),
    })
}

/// The next double-quoted string, unescaped, and what follows it.
fn quoted(text: &str) -> Option<(String, &str)> {
    let body = text.trim_start().strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = body.char_indices();
    while let Some((index, c)) = chars.next() {
        match c {
            '\\' => {
                if let Some((_, escaped)) = chars.next() {
                    out.push(escaped);
                }
            }
            '"' => return Some((out, &body[index + 1..])),
            other => out.push(other),
        }
    }
    None
}

/// The next whitespace-separated token and what follows it.
fn token(text: &str) -> Option<(&str, &str)> {
    let text = text.trim_start();
    if text.is_empty() {
        return None;
    }
    let end = text.find(char::is_whitespace).unwrap_or(text.len());
    Some((&text[..end], &text[end..]))
}

/// A request target as a path without query or fragment; `None` when it is not one.
fn clean_path(target: &str) -> Option<String> {
    let target = match target.find("://") {
        Some(scheme) => {
            let after = &target[scheme + 3..];
            &after[after.find('/')?..]
        }
        None => target,
    };
    let path = target.split(['?', '#']).next().unwrap_or_default();
    (path.starts_with('/') && !path.chars().any(char::is_control)).then(|| path.to_string())
}

/// Decodes `%XX` escapes, and `+` as a space when `plus` is set.
fn percent_decode(text: &str, plus: bool) -> String {
    let hex = |byte: u8| char::from(byte).to_digit(16).and_then(|digit| u8::try_from(digit).ok());
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                if let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2])) {
                    out.push(high * 16 + low);
                    index += 3;
                    continue;
                }
                out.push(b'%');
            }
            b'+' if plus => out.push(b' '),
            byte => out.push(byte),
        }
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// What one [`HostLogs::ingest`] read (spec 3.5 §7.12).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestReport {
    /// Requests counted.
    pub counted: u64,
    /// Lines in no format the parser reads.
    pub skipped: u64,
    /// The earliest request counted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first: Option<DateTime<Utc>>,
    /// The latest request counted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<DateTime<Utc>>,
}

/// Host access logs as the `host_logs` KPI source (spec 3.5 §7.12): each ingested request
/// adds to the daily counts of its day, and nothing else of it is kept — no address, no
/// query string, no referrer.
///
/// | Metric | Counts |
/// |---|---|
/// | `requests`, `bytes` | Every request, and the bytes sent |
/// | `status:2xx` … `status:5xx`, `not_found` | Requests by status class; 404s |
/// | `pageviews`, `pageviews:{path}` | `GET`s of a page (a path ending in `/`, `.html` or with no extension) answered 200 or 304, by people |
/// | `crawlers` | Requests whose user agent claims an automated client |
/// | `agents`, `agents:{name}` | Requests whose user agent names an AI crawler or assistant, such as `agents:gptbot` |
/// | `machine_views`, `agents.machine_views` | Requests for machine views — `.awp.json` and `.md` twins, `llms.txt`, `/.well-known/awp…`, the agent card — all, and by AI agents |
///
/// A user agent is a claim: these are self-declared crawlers and agents. Agents with a
/// verified identity are counted at the edge ([`AgentSearchKpi`](super::AgentSearchKpi)).
/// Ingest each log once: a line ingested twice is counted twice.
pub struct HostLogs {
    site: Uuid,
    counts: Arc<dyn BeaconStore>,
    parser: Mutex<HostLogParser>,
}

impl std::fmt::Debug for HostLogs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostLogs").field("site", &self.site).finish_non_exhaustive()
    }
}

impl HostLogs {
    /// `site`'s host logs, counted in `counts`.
    pub fn new(site: Uuid, counts: Arc<dyn BeaconStore>) -> Self {
        Self { site, counts, parser: Mutex::new(HostLogParser::new()) }
    }

    /// Counts every request in `text`, a chunk of a log; a `#Fields:` line carries over to
    /// the next chunk.
    ///
    /// # Errors
    ///
    /// Returns the count store's error; nothing of the chunk is counted then.
    pub async fn ingest(&self, text: &str) -> adk_kernel::Result<IngestReport> {
        let mut parser = self.parser.lock().await;
        let mut tally = Tally::default();
        let mut report = IngestReport::default();
        for line in text.lines() {
            let directive = line.trim().is_empty() || line.starts_with('#');
            match parser.parse_line(line) {
                Some(request) => {
                    count(&mut tally, &request);
                    report.counted += 1;
                    report.first = Some(report.first.map_or(request.at, |at| at.min(request.at)));
                    report.last = Some(report.last.map_or(request.at, |at| at.max(request.at)));
                }
                None if directive => {}
                None => report.skipped += 1,
            }
        }
        drop(parser);
        tally.flush("host_logs", self.counts.as_ref()).await?;
        tracing::debug!(site.id = %self.site, counted = report.counted, skipped = report.skipped, "host log ingested");
        Ok(report)
    }
}

/// Adds one request to the day's counts.
fn count(tally: &mut Tally, request: &LogLine) {
    let day: NaiveDate = request.at.date_naive();
    tally.add(day, "requests", 1);
    tally.add(day, "bytes", request.bytes);
    tally.add(day, format!("status:{}xx", request.status / 100), 1);
    if request.status == 404 {
        tally.add(day, "not_found", 1);
    }
    let agent = request.user_agent.as_deref().map(str::to_ascii_lowercase).unwrap_or_default();
    let named = AGENTS.iter().find(|name| agent.contains(*name));
    let automated = named.is_some() || CRAWLERS.iter().any(|marker| agent.contains(marker));
    if automated {
        tally.add(day, "crawlers", 1);
    }
    if let Some(name) = named {
        tally.add(day, "agents", 1);
        tally.add(day, format!("agents:{name}"), 1);
    }
    let path = request.path.as_str();
    let machine_view = path.ends_with(".awp.json")
        || path.ends_with(".md")
        || matches!(path, "/llms.txt" | "/llms-full.txt" | "/.well-known/agent-card.json")
        || path.starts_with("/.well-known/awp");
    if machine_view {
        tally.add(day, "machine_views", 1);
        if named.is_some() {
            tally.add(day, "agents.machine_views", 1);
        }
    }
    let last = path.rsplit('/').next().unwrap_or_default();
    let page = path.ends_with('/')
        || path.ends_with(".html")
        || path.ends_with(".htm")
        || (!last.contains('.') && !machine_view);
    if page && request.method == "GET" && matches!(request.status, 200 | 304) && !automated {
        tally.add(day, "pageviews", 1);
        tally.add(day, format!("pageviews:{path}"), 1);
    }
}

#[async_trait]
impl KpiSource for HostLogs {
    fn name(&self) -> &str {
        "host_logs"
    }

    async fn read(
        &self,
        site: Uuid,
        metric: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> adk_kernel::Result<Option<f64>> {
        check_site("host_logs", self.site, site)?;
        total("host_logs", &self.counts, metric, from, to).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combined_lines_keep_no_address_or_query() {
        let mut parser = HostLogParser::new();
        let line = parser
            .parse_line(
                r#"2001:db8::1 - frank [10/Oct/2026:13:55:36 -0700] "GET /apache_pb.gif?id=7 HTTP/1.0" 200 2326 "http://www.example.com/start.html" "Mozilla/4.08 [en] (Win98; I ;Nav)""#,
            )
            .unwrap();
        assert_eq!(line.path, "/apache_pb.gif");
        assert_eq!(line.user_agent.as_deref(), Some("Mozilla/4.08 [en] (Win98; I ;Nav)"));
        assert_eq!(line.at.to_rfc3339(), "2026-10-10T20:55:36+00:00");
        let json = serde_json::to_string(&line).unwrap();
        assert!(!json.contains("2001:db8") && !json.contains("frank") && !json.contains("id=7"));
    }

    #[test]
    fn escaped_quotes_and_dashes_parse() {
        let mut parser = HostLogParser::new();
        let line = parser
            .parse_line(r#"h - - [10/Oct/2026:13:55:36 +0000] "GET http://example.com/x/ HTTP/1.1" 304 - "-" "a \"quoted\" agent""#)
            .unwrap();
        assert_eq!((line.path.as_str(), line.bytes), ("/x/", 0));
        assert_eq!(line.user_agent.as_deref(), Some("a \"quoted\" agent"));
        assert!(parser.parse_line(r#"h - - [10/Oct/2026:13:55:36 +0000] "-" 400 0"#).is_none());
        assert!(parser.parse_line("garbage").is_none());
    }

    #[test]
    fn percent_decoding_stops_at_the_end() {
        assert_eq!(percent_decode("a%20b+c%2", true), "a b c%2");
        assert_eq!(percent_decode("%zz", false), "%zz");
    }
}
