//! A calendar driver over CalDAV (RFC 4791), which Nextcloud, Fastmail, iCloud, Radicale,
//! Baïkal, Google Calendar and most calendar servers speak (spec 3.5 §7.5 "Commerce",
//! §7.16 `booking`).
//!
//! | Collection | Holds |
//! |---|---|
//! | Availability | The owner's open slots, one event each; a recurring event is a slot per occurrence |
//! | Bookings | The bookings the site writes, one event per booked slot; by default the availability collection itself. A separate bookings collection, such as the owner's own calendar, also blocks every slot its busy events overlap |
//!
//! | Calendar call | CalDAV |
//! |---|---|
//! | [`open`](Calendar::open) | `REPORT` `calendar-query` with a `time-range` on both collections, recurring events expanded by the server (RFC 4791 §9.6.5); a slot is open while no booking or busy event overlaps it |
//! | [`take`](Calendar::take) | `PUT` of the slot's booking with `If-None-Match: *` to a name derived from the slot, so two sites, processes or callers never book one slot twice; a `412` answer reads the booking back and answers `true` only for the same booking key |
//! | [`price`](Calendar::price) | The slot's `X-ADK-PRICE` (minor units) as last listed, else the calendar's price |
//!
//! A booking event carries the slot (`X-ADK-SLOT`) and the booking key
//! (`X-ADK-BOOKING-KEY`), is opaque and confirmed, and holds no personal data: the contact
//! is in the site's contact book. Credentials travel with every request, so the server must
//! be `https` (plain `http` on loopback only).

use std::collections::BTreeMap;
use std::sync::PoisonError;
use std::time::Duration;

use adk_core::{AdkError, CredentialSecret, ErrorCategory, ErrorComponent, Result};
use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use quick_xml::Reader;
use quick_xml::events::Event;
use reqwest::header::{CONTENT_TYPE, HeaderValue};
use url::Url;

use super::handlers::{Calendar, Slot};

/// The longest answer read from a server.
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// The longest window one listing reads.
const MAX_WINDOW_DAYS: i64 = 366;

/// The slot a booking event books.
const SLOT_PROPERTY: &str = "X-ADK-SLOT";
/// The booking key a booking event was taken with.
const KEY_PROPERTY: &str = "X-ADK-BOOKING-KEY";
/// A slot's price in minor units of the site's currency.
const PRICE_PROPERTY: &str = "X-ADK-PRICE";

fn error(category: ErrorCategory, code: &'static str, message: impl Into<String>) -> AdkError {
    AdkError::new(ErrorComponent::Tool, category, code, message)
}

fn unavailable(message: impl Into<String>) -> AdkError {
    error(ErrorCategory::Unavailable, "website.caldav.unavailable", message)
}

/// How the driver signs in to the CalDAV server.
#[derive(Clone)]
#[non_exhaustive]
pub enum CalDavAuth {
    /// HTTP Basic with an app password, as most servers issue for calendar clients.
    Basic {
        /// The account.
        username: String,
        /// The app password.
        password: CredentialSecret,
    },
    /// An OAuth bearer token, as Google Calendar's CalDAV endpoint takes.
    Bearer(CredentialSecret),
    /// No credentials, for a server on loopback that asks for none.
    None,
}

impl std::fmt::Debug for CalDavAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Basic { username, .. } => {
                f.debug_struct("Basic").field("username", username).finish_non_exhaustive()
            }
            Self::Bearer(_) => f.write_str("Bearer([redacted])"),
            Self::None => f.write_str("None"),
        }
    }
}

/// A slot as last read: what booking and pricing it need.
#[derive(Debug, Clone)]
struct Known {
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    summary: Option<String>,
    price: Option<u64>,
}

/// The owner's bookable slots in a CalDAV calendar (spec 3.5 §7.5 "Commerce", §7.16
/// `booking`). See the [module documentation](self) for the collections and the protocol.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
///
/// use adk_core::CredentialSecret;
/// use adk_website::edge::caldav::{CalDavAuth, CalDavCalendar};
/// use adk_website::edge::handlers::Calendar;
///
/// # fn demo() -> adk_core::Result<()> {
/// let auth = CalDavAuth::Basic {
///     username: "juniper".into(),
///     password: CredentialSecret::new(std::env::var("CALDAV_APP_PASSWORD").unwrap_or_default()),
/// };
/// let calendar = CalDavCalendar::new("https://dav.example.com/calendars/juniper/open-slots/", auth)?
///     .with_bookings("https://dav.example.com/calendars/juniper/personal/")?
///     .with_price(1_500);
/// let calendar: Arc<dyn Calendar> = Arc::new(calendar);
/// # let _ = calendar;
/// # Ok(())
/// # }
/// ```
pub struct CalDavCalendar {
    http: reqwest::Client,
    availability: Url,
    bookings: Url,
    auth: CalDavAuth,
    zone: Tz,
    price: Option<u64>,
    known: std::sync::Mutex<BTreeMap<String, Known>>,
}

impl std::fmt::Debug for CalDavCalendar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CalDavCalendar")
            .field("availability", &self.availability.as_str())
            .field("bookings", &self.bookings.as_str())
            .field("auth", &self.auth)
            .finish_non_exhaustive()
    }
}

/// `address` as a collection URL ending in `/`: `https`, or `http` on loopback only.
fn collection(address: &str) -> Result<Url> {
    let invalid =
        |reason: String| error(ErrorCategory::InvalidInput, "website.caldav.address", reason);
    let mut url = Url::parse(address)
        .map_err(|e| invalid(format!("`{address}` is not a calendar address: {e}")))?;
    let loopback = url.host_str().is_some_and(|host| {
        host == "localhost"
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    });
    match url.scheme() {
        "https" => {}
        "http" if loopback => {}
        _ => {
            return Err(invalid(format!(
                "`{address}` must be https (plain http only on loopback), since the calendar's \
                 credentials travel with each request"
            )));
        }
    }
    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }
    Ok(url)
}

impl CalDavCalendar {
    /// The slots in the calendar collection at `availability`, booked into the same
    /// collection, signed in with `auth`.
    ///
    /// # Errors
    ///
    /// An address that is not `https` (or `http` on loopback); a client that cannot be built.
    pub fn new(availability: &str, auth: CalDavAuth) -> Result<Self> {
        adk_core::ensure_crypto_provider();
        let availability = collection(availability)?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("adk-website/", env!("CARGO_PKG_VERSION"), " caldav"))
            .build()
            .map_err(|e| unavailable(format!("the CalDAV client could not be built: {e}")))?;
        Ok(Self {
            http,
            bookings: availability.clone(),
            availability,
            auth,
            zone: Tz::UTC,
            price: None,
            known: std::sync::Mutex::new(BTreeMap::new()),
        })
    }

    /// Writes bookings to the collection at `bookings`, such as the owner's own calendar,
    /// whose busy events then also block the slots they overlap.
    ///
    /// # Errors
    ///
    /// An address that is not `https` (or `http` on loopback).
    pub fn with_bookings(mut self, bookings: &str) -> Result<Self> {
        self.bookings = collection(bookings)?;
        Ok(self)
    }

    /// Prices a slot with no `X-ADK-PRICE` of its own at `minor` units of the site's
    /// currency, for the owner's amount thresholds (spec 3.5 §7.4); slots are free
    /// otherwise.
    #[must_use]
    pub fn with_price(mut self, minor: u64) -> Self {
        self.price = Some(minor);
        self
    }

    /// Reads times with no zone, and all-day events, in `zone`; UTC by default.
    #[must_use]
    pub fn with_time_zone(mut self, zone: Tz) -> Self {
        self.zone = zone;
        self
    }

    fn separate(&self) -> bool {
        self.bookings != self.availability
    }

    fn authorized(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.auth {
            CalDavAuth::Basic { username, password } => {
                request.basic_auth(username, Some(password.expose()))
            }
            CalDavAuth::Bearer(token) => request.bearer_auth(token.expose()),
            CalDavAuth::None => request,
        }
    }

    /// Sends `request` and reads its answer, bounded.
    async fn send(&self, request: reqwest::RequestBuilder, url: &Url) -> Result<(u16, String)> {
        let response = self.authorized(request).send().await.map_err(|e| {
            unavailable(format!("the calendar at {url} did not answer: {}", e.without_url()))
        })?;
        let status = response.status().as_u16();
        let body = response.bytes().await.map_err(|e| {
            unavailable(format!("the calendar at {url} did not answer: {}", e.without_url()))
        })?;
        if body.len() > MAX_BODY_BYTES {
            return Err(unavailable(format!("the calendar at {url} answered more than 8 MiB")));
        }
        Ok((status, String::from_utf8_lossy(&body).into_owned()))
    }

    /// The error a failed answer means.
    fn failed(url: &Url, status: u16, body: &str) -> AdkError {
        let detail: String = body.trim().chars().take(300).collect();
        match status {
            401 | 403 => error(
                ErrorCategory::Unauthorized,
                "website.caldav.unauthorized",
                format!(
                    "the calendar at {url} refused the site's credentials ({status}); issue a \
                     new app password or token for it"
                ),
            ),
            404 => error(
                ErrorCategory::NotFound,
                "website.caldav.not_found",
                format!("no calendar collection at {url}; check the address the owner gave"),
            ),
            _ => unavailable(format!("the calendar at {url} answered {status}: {detail}")),
        }
    }

    /// The events of `collection` that `filter` selects, recurring ones expanded over
    /// `[from, to)` when given.
    async fn report(
        &self,
        collection: &Url,
        filter: &str,
        expand: Option<(DateTime<Utc>, DateTime<Utc>)>,
    ) -> Result<Vec<CalEvent>> {
        let data = match expand {
            Some((from, to)) => format!(
                r#"<C:calendar-data><C:expand start="{}" end="{}"/></C:calendar-data>"#,
                stamp(from),
                stamp(to)
            ),
            None => "<C:calendar-data/>".to_string(),
        };
        let body = format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<C:calendar-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:prop><D:getetag/>{data}</D:prop>
  <C:filter><C:comp-filter name="VCALENDAR"><C:comp-filter name="VEVENT">{filter}</C:comp-filter></C:comp-filter></C:filter>
</C:calendar-query>"#
        );
        let method = reqwest::Method::from_bytes(b"REPORT").expect("a valid method");
        let request = self
            .http
            .request(method, collection.clone())
            .header("Depth", "1")
            .header(CONTENT_TYPE, HeaderValue::from_static("application/xml; charset=utf-8"))
            .body(body);
        let (status, text) = self.send(request, collection).await?;
        if status != 207 {
            return Err(Self::failed(collection, status, &text));
        }
        let mut events = Vec::new();
        for calendar in calendar_data(&text)? {
            events.extend(parse_events(&calendar, self.zone));
        }
        Ok(events)
    }

    /// The events of `collection` overlapping `[from, to)`, recurring ones expanded.
    async fn events(
        &self,
        collection: &Url,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<CalEvent>> {
        let filter = format!(r#"<C:time-range start="{}" end="{}"/>"#, stamp(from), stamp(to));
        self.report(collection, &filter, Some((from, to))).await
    }

    /// Whether `event`, read from the bookings collection, makes its time busy.
    fn busy(&self, event: &CalEvent) -> bool {
        !event.cancelled && (event.slot.is_some() || (self.separate() && !event.transparent))
    }

    /// The slot `id`, read from the availability collection.
    async fn find(&self, id: &str) -> Result<Option<Known>> {
        if let Some(known) = self.known.lock().unwrap_or_else(PoisonError::into_inner).get(id) {
            return Ok(Some(known.clone()));
        }
        let (uid, instance) = match id.rsplit_once('#') {
            Some((uid, at)) => match parse_utc(at) {
                Some(at) => (uid, Some(at)),
                None => (id, None),
            },
            None => (id, None),
        };
        let events = match instance {
            Some(at) => {
                self.events(&self.availability, at, at + chrono::Duration::minutes(1)).await?
            }
            None => {
                let filter = format!(
                    r#"<C:prop-filter name="UID"><C:text-match collation="i;octet">{}</C:text-match></C:prop-filter>"#,
                    xml_escape(uid)
                );
                self.report(&self.availability, &filter, None).await?
            }
        };
        Ok(events
            .into_iter()
            .find(|event| event.slot.is_none() && !event.cancelled && event.id() == id)
            .map(|event| self.remember(&event)))
    }

    fn remember(&self, event: &CalEvent) -> Known {
        let known = Known {
            starts_at: event.starts_at,
            ends_at: event.ends_at,
            summary: event.summary.clone(),
            price: event.price.or(self.price),
        };
        self.known.lock().unwrap_or_else(PoisonError::into_inner).insert(event.id(), known.clone());
        known
    }

    /// Where the booking of slot `id` is written: one name per slot.
    fn booking_url(&self, id: &str) -> Result<Url> {
        let name = format!(
            "adk-booking-{}.ics",
            &adk_site::release::file_digest(id.as_bytes()).hex()[..32]
        );
        self.bookings
            .join(&name)
            .map_err(|e| unavailable(format!("no booking address under {}: {e}", self.bookings)))
    }

    /// The booking key of the booking at `url`; `None` when there is none.
    async fn booking_key(&self, url: &Url) -> Result<Option<String>> {
        let (status, text) = self.send(self.http.get(url.clone()), url).await?;
        match status {
            200 => Ok(parse_events(&text, self.zone).into_iter().find_map(|event| event.key)),
            404 | 410 => Ok(None),
            _ => Err(Self::failed(url, status, &text)),
        }
    }
}

#[async_trait]
impl Calendar for CalDavCalendar {
    async fn open(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<Slot>> {
        if to <= from {
            return Ok(Vec::new());
        }
        let to = to.min(from + chrono::Duration::days(MAX_WINDOW_DAYS));
        let listed = self.events(&self.availability, from, to).await?;
        let mut busy: Vec<CalEvent> = listed
            .iter()
            .filter(|event| event.slot.is_some() && !event.cancelled)
            .cloned()
            .collect();
        if self.separate() {
            busy = self.events(&self.bookings, from, to).await?;
        }
        busy.retain(|event| self.busy(event));
        let mut slots = Vec::new();
        for event in listed.iter().filter(|event| event.slot.is_none() && !event.cancelled) {
            if event.starts_at < from || event.starts_at >= to {
                continue;
            }
            let known = self.remember(event);
            if busy.iter().any(|b| b.starts_at < event.ends_at && b.ends_at > event.starts_at) {
                continue;
            }
            let minutes = (event.ends_at - event.starts_at).num_minutes().max(0);
            slots.push(Slot {
                id: event.id(),
                starts_at: event.starts_at,
                minutes: u32::try_from(minutes).unwrap_or(u32::MAX),
                price: known.price,
            });
        }
        slots.sort_by(|a, b| a.starts_at.cmp(&b.starts_at).then_with(|| a.id.cmp(&b.id)));
        Ok(slots)
    }

    async fn take(&self, id: &str, key: &str) -> Result<bool> {
        let Some(slot) = self.find(id).await? else { return Ok(false) };
        let url = self.booking_url(id)?;
        let conflicts = self.events(&self.bookings, slot.starts_at, slot.ends_at).await?;
        for event in conflicts.iter().filter(|event| self.busy(event)) {
            if event.starts_at >= slot.ends_at || event.ends_at <= slot.starts_at {
                continue;
            }
            return Ok(event.slot.as_deref() == Some(id) && event.key.as_deref() == Some(key));
        }
        let body = booking(id, key, &slot, Utc::now());
        let request = self
            .http
            .put(url.clone())
            .header("If-None-Match", "*")
            .header(CONTENT_TYPE, HeaderValue::from_static("text/calendar; charset=utf-8"))
            .body(body);
        let (status, text) = self.send(request, &url).await?;
        match status {
            200..=299 => {
                tracing::info!(website.booking.slot = id, "slot booked in the calendar");
                Ok(true)
            }
            // Booked meanwhile, by this key's earlier attempt or by someone else.
            412 => Ok(self.booking_key(&url).await?.as_deref() == Some(key)),
            _ => Err(Self::failed(&url, status, &text)),
        }
    }

    fn price(&self, id: &str) -> Option<u64> {
        match self.known.lock().unwrap_or_else(PoisonError::into_inner).get(id) {
            Some(known) => known.price,
            None => self.price,
        }
    }
}

/// An event of a calendar, at the level a booking needs.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CalEvent {
    uid: String,
    recurrence: Option<DateTime<Utc>>,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    summary: Option<String>,
    transparent: bool,
    cancelled: bool,
    slot: Option<String>,
    key: Option<String>,
    price: Option<u64>,
}

impl CalEvent {
    /// The slot id: the UID, with the occurrence for an instance of a recurring event.
    fn id(&self) -> String {
        match self.recurrence {
            Some(at) => format!("{}#{}", self.uid, stamp(at)),
            None => self.uid.clone(),
        }
    }
}

/// `at` as an iCalendar UTC date-time.
fn stamp(at: DateTime<Utc>) -> String {
    at.format("%Y%m%dT%H%M%SZ").to_string()
}

fn parse_utc(text: &str) -> Option<DateTime<Utc>> {
    NaiveDateTime::parse_from_str(text.trim_end_matches('Z'), "%Y%m%dT%H%M%S")
        .ok()
        .filter(|_| text.ends_with('Z'))
        .map(|at| at.and_utc())
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The `calendar-data` of every response in a `multistatus` answer, whatever namespace
/// prefixes the server writes.
fn calendar_data(xml: &str) -> Result<Vec<String>> {
    let malformed = |e: &dyn std::fmt::Display| {
        unavailable(format!("the calendar's answer is not a CalDAV multistatus: {e}"))
    };
    let mut reader = Reader::from_str(xml);
    let mut found = Vec::new();
    let mut current: Option<String> = None;
    loop {
        match reader.read_event().map_err(|e| malformed(&e))? {
            Event::Start(start) if start.local_name().as_ref() == b"calendar-data" => {
                current = Some(String::new());
            }
            Event::End(end) if end.local_name().as_ref() == b"calendar-data" => {
                if let Some(text) = current.take() {
                    found.push(text);
                }
            }
            Event::Text(text) => {
                if let Some(current) = current.as_mut() {
                    current.push_str(&text.xml10_content().map_err(|e| malformed(&e))?);
                }
            }
            Event::CData(data) => {
                if let Some(current) = current.as_mut() {
                    current.push_str(&data.decode().map_err(|e| malformed(&e))?);
                }
            }
            Event::GeneralRef(reference) => {
                if let Some(current) = current.as_mut() {
                    let resolved = if reference.is_char_ref() {
                        reference.resolve_char_ref().map_err(|e| malformed(&e))?
                    } else {
                        match reference.decode().map_err(|e| malformed(&e))?.as_ref() {
                            "amp" => Some('&'),
                            "lt" => Some('<'),
                            "gt" => Some('>'),
                            "quot" => Some('"'),
                            "apos" => Some('\''),
                            _ => None,
                        }
                    };
                    current.extend(resolved);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(found)
}

/// iCalendar text with its folded lines joined (RFC 5545 §3.1).
fn unfold(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        match (line.strip_prefix([' ', '\t']), lines.last_mut()) {
            (Some(rest), Some(last)) => last.push_str(rest),
            _ if line.is_empty() => {}
            _ => lines.push(line.to_string()),
        }
    }
    lines
}

/// A property line: its name, parameters and value.
fn property(line: &str) -> Option<(String, BTreeMap<String, String>, String)> {
    let mut quoted = false;
    let split = line.char_indices().find(|(_, c)| {
        if *c == '"' {
            quoted = !quoted;
        }
        *c == ':' && !quoted
    })?;
    let (head, value) = (&line[..split.0], &line[split.0 + 1..]);
    let mut parts = head.split(';');
    let name = parts.next()?.trim().to_ascii_uppercase();
    let params = parts
        .filter_map(|param| param.split_once('='))
        .map(|(k, v)| (k.trim().to_ascii_uppercase(), v.trim().trim_matches('"').to_string()))
        .collect();
    Some((name, params, value.to_string()))
}

/// An iCalendar TEXT value unescaped (RFC 5545 §3.3.11).
fn text_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n' | 'N') => out.push('\n'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// A TEXT value escaped and a property line folded at 75 octets (RFC 5545 §3.1, §3.3.11).
fn text_line(name: &str, value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace('\n', "\\n")
        .replace('\r', "");
    let line = format!("{name}:{escaped}");
    let mut folded = String::new();
    let mut width = 0;
    for c in line.chars() {
        if width + c.len_utf8() > 75 {
            folded.push_str("\r\n ");
            width = 1;
        }
        folded.push(c);
        width += c.len_utf8();
    }
    folded.push_str("\r\n");
    folded
}

/// A DATE or DATE-TIME value as UTC, with whether it was a whole day.
fn date_time(
    value: &str,
    params: &BTreeMap<String, String>,
    zone: Tz,
) -> Option<(DateTime<Utc>, bool)> {
    let value = value.trim();
    if params.get("VALUE").is_some_and(|v| v.eq_ignore_ascii_case("DATE")) || value.len() == 8 {
        let day = NaiveDate::parse_from_str(value, "%Y%m%d").ok()?;
        let local = zone.from_local_datetime(&day.and_hms_opt(0, 0, 0)?).earliest()?;
        return Some((local.with_timezone(&Utc), true));
    }
    if value.ends_with('Z') {
        return parse_utc(value).map(|at| (at, false));
    }
    let naive = NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S").ok()?;
    let zone = match params.get("TZID") {
        // Some servers prefix the IANA name, as `/mozilla.org/20050126_1/Europe/London`.
        Some(tzid) => tzid.parse::<Tz>().ok().or_else(|| {
            let mut parts = tzid.rsplit('/');
            let (city, region) = (parts.next()?, parts.next()?);
            format!("{region}/{city}").parse::<Tz>().ok()
        })?,
        None => zone,
    };
    let local = zone.from_local_datetime(&naive).earliest()?;
    Some((local.with_timezone(&Utc), false))
}

/// A DURATION value (RFC 5545 §3.3.6): `P1D`, `PT1H30M`, `P1W`.
fn duration(value: &str) -> Option<chrono::Duration> {
    let (negative, rest) = match value.trim().strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, value.trim().trim_start_matches('+')),
    };
    let rest = rest.strip_prefix('P')?;
    let mut total = chrono::Duration::zero();
    let mut number = String::new();
    let mut time = false;
    for c in rest.chars() {
        match c {
            'T' => time = true,
            '0'..='9' => number.push(c),
            unit => {
                let n: i64 = std::mem::take(&mut number).parse().ok()?;
                total += match (unit, time) {
                    ('W', false) => chrono::Duration::weeks(n),
                    ('D', false) => chrono::Duration::days(n),
                    ('H', true) => chrono::Duration::hours(n),
                    ('M', true) => chrono::Duration::minutes(n),
                    ('S', true) => chrono::Duration::seconds(n),
                    _ => return None,
                };
            }
        }
    }
    Some(if negative { -total } else { total })
}

/// The events of an iCalendar object. An event whose times do not read, or a recurring
/// event the server did not expand, is left out with a warning.
fn parse_events(text: &str, zone: Tz) -> Vec<CalEvent> {
    let mut events = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    let mut props: Vec<(String, BTreeMap<String, String>, String)> = Vec::new();
    for line in unfold(text) {
        let Some((name, params, value)) = property(&line) else { continue };
        match name.as_str() {
            "BEGIN" => {
                stack.push(value.trim().to_ascii_uppercase());
                if stack.last().is_some_and(|c| c == "VEVENT") {
                    props.clear();
                }
            }
            "END" => {
                if stack.pop().is_some_and(|c| c == "VEVENT")
                    && let Some(event) = event(&props, zone)
                {
                    events.push(event);
                }
            }
            _ if stack.last().is_some_and(|c| c == "VEVENT") => props.push((name, params, value)),
            _ => {}
        }
    }
    events
}

fn event(props: &[(String, BTreeMap<String, String>, String)], zone: Tz) -> Option<CalEvent> {
    let get = |name: &str| props.iter().find(|(n, _, _)| n == name);
    let uid = text_value(&get("UID")?.2);
    let recurrence = match get("RECURRENCE-ID") {
        Some((_, params, value)) => Some(date_time(value, params, zone)?.0),
        None => None,
    };
    if recurrence.is_none() && (get("RRULE").is_some() || get("RDATE").is_some()) {
        tracing::warn!(
            caldav.uid = %uid,
            "a recurring event came back unexpanded; the server does not expand recurrences (RFC 4791 §9.6.5), so it is left out"
        );
        return None;
    }
    let Some((starts_at, whole_day)) =
        get("DTSTART").and_then(|(_, params, value)| date_time(value, params, zone))
    else {
        tracing::warn!(caldav.uid = %uid, "an event whose start does not read is left out");
        return None;
    };
    let ends_at = match (get("DTEND"), get("DURATION")) {
        (Some((_, params, value)), _) => date_time(value, params, zone)?.0,
        (None, Some((_, _, value))) => starts_at + duration(value)?,
        (None, None) if whole_day => starts_at + chrono::Duration::days(1),
        (None, None) => starts_at,
    };
    let flag = |name: &str, expected: &str| {
        get(name).is_some_and(|(_, _, value)| value.trim().eq_ignore_ascii_case(expected))
    };
    Some(CalEvent {
        uid,
        recurrence,
        starts_at,
        ends_at,
        summary: get("SUMMARY").map(|(_, _, value)| text_value(value)),
        transparent: flag("TRANSP", "TRANSPARENT"),
        cancelled: flag("STATUS", "CANCELLED"),
        slot: get(SLOT_PROPERTY).map(|(_, _, value)| text_value(value)),
        key: get(KEY_PROPERTY).map(|(_, _, value)| text_value(value)),
        price: get(PRICE_PROPERTY).and_then(|(_, _, value)| value.trim().parse().ok()),
    })
}

/// The booking event of slot `id` taken with `key`.
fn booking(id: &str, key: &str, slot: &Known, now: DateTime<Utc>) -> String {
    let uid = format!("adk-booking-{}", &adk_site::release::file_digest(id.as_bytes()).hex()[..32]);
    let summary = match &slot.summary {
        Some(summary) => format!("Booked: {summary}"),
        None => "Booked".to_string(),
    };
    let mut text = String::from(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//adk-rust//adk-website//EN\r\nBEGIN:VEVENT\r\n",
    );
    text.push_str(&text_line("UID", &uid));
    text.push_str(&format!("DTSTAMP:{}\r\n", stamp(now)));
    text.push_str(&format!("DTSTART:{}\r\n", stamp(slot.starts_at)));
    text.push_str(&format!("DTEND:{}\r\n", stamp(slot.ends_at)));
    text.push_str(&text_line("SUMMARY", &summary));
    text.push_str("TRANSP:OPAQUE\r\nSTATUS:CONFIRMED\r\n");
    text.push_str(&text_line(SLOT_PROPERTY, id));
    text.push_str(&text_line(KEY_PROPERTY, key));
    text.push_str("END:VEVENT\r\nEND:VCALENDAR\r\n");
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folded_escaped_text_round_trips() {
        let slot = Known {
            starts_at: Utc.with_ymd_and_hms(2026, 11, 3, 9, 0, 0).unwrap(),
            ends_at: Utc.with_ymd_and_hms(2026, 11, 3, 9, 30, 0).unwrap(),
            summary: Some("Tasting; bread, cake \\ and a very long description that folds".into()),
            price: None,
        };
        let id = "weekly@juniper.example#20261103T090000Z";
        let text = booking(id, "b-1;x", &slot, slot.starts_at);
        assert!(text.lines().all(|line| line.len() <= 76), "{text}");
        let events = parse_events(&text, Tz::UTC);
        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!((event.slot.as_deref(), event.key.as_deref()), (Some(id), Some("b-1;x")));
        assert_eq!(
            event.summary.as_deref(),
            Some("Booked: Tasting; bread, cake \\ and a very long description that folds")
        );
        assert_eq!((event.starts_at, event.ends_at), (slot.starts_at, slot.ends_at));
        assert!(!event.transparent && !event.cancelled);
    }

    #[test]
    fn times_read_in_their_zone_and_alarms_do_not_leak() {
        let text = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:a\nDTSTART;TZID=Africa/Nairobi:20261103T120000\nDURATION:PT1H30M\nBEGIN:VALARM\nTRIGGER:-PT15M\nDURATION:PT5M\nEND:VALARM\nEND:VEVENT\nBEGIN:VEVENT\nUID:b\nDTSTART;VALUE=DATE:20261104\nEND:VEVENT\nBEGIN:VEVENT\nUID:c\nDTSTART:20261105T080000Z\nRRULE:FREQ=WEEKLY\nEND:VEVENT\nEND:VCALENDAR\n";
        let events = parse_events(text, "Europe/London".parse().unwrap());
        assert_eq!(events.len(), 2, "the unexpanded recurring event is left out: {events:?}");
        assert_eq!(events[0].starts_at, Utc.with_ymd_and_hms(2026, 11, 3, 9, 0, 0).unwrap());
        assert_eq!(events[0].ends_at, Utc.with_ymd_and_hms(2026, 11, 3, 10, 30, 0).unwrap());
        assert_eq!(events[1].starts_at, Utc.with_ymd_and_hms(2026, 11, 4, 0, 0, 0).unwrap());
        assert_eq!(events[1].ends_at - events[1].starts_at, chrono::Duration::days(1));
        assert_eq!(duration("P1W"), Some(chrono::Duration::weeks(1)));
        assert_eq!(duration("PT1H"), Some(chrono::Duration::hours(1)));
        assert_eq!(duration("1H"), None);
    }

    #[test]
    fn only_https_or_loopback_carries_credentials() {
        assert!(collection("https://dav.example.com/cal").unwrap().as_str().ends_with("/cal/"));
        assert!(collection("http://127.0.0.1:5232/u/c/").is_ok());
        assert!(collection("http://dav.example.com/cal/").is_err());
    }
}
