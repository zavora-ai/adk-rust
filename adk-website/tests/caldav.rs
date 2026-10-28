//! The CalDAV calendar driver (spec 3.5 §7.5 "Commerce", §7.16 `booking`) against a CalDAV
//! server double that answers `REPORT` with recorded multistatus responses — the RFC 4791
//! §7.8.3 shape with `D:`/`C:` prefixes, and the `d:`/`cal:` shape with escaped CRLF line
//! ends that Nextcloud and Radicale send — and keeps `PUT` resources with `If-None-Match`.
//! A live run against a real server is `#[ignore]`d.

mod common;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};

use adk_core::{CredentialSecret, ErrorCategory};
use adk_website::edge::caldav::{CalDavAuth, CalDavCalendar};
use adk_website::edge::handlers::Calendar;
use axum::body::Bytes;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use chrono::{TimeZone, Utc};

const SLOTS: &str = "/calendars/juniper/open-slots/";
const PERSONAL: &str = "/remote.php/dav/calendars/juniper/personal/";
const PASSWORD: &str = "app-password-1";

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/caldav").join(name);
    std::fs::read_to_string(path).unwrap()
}

/// One request the server saw.
#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    depth: Option<String>,
    if_none_match: Option<String>,
    body: String,
}

#[derive(Default)]
struct Dav {
    recorded: BTreeMap<String, String>,
    stored: BTreeMap<String, String>,
    seen: Vec<Seen>,
    /// Leaves stored bookings out of `REPORT` answers, as a lagging index would.
    hide_stored: bool,
}

/// A CalDAV server double on loopback, stopped on drop.
struct Server {
    addr: SocketAddr,
    dav: Arc<Mutex<Dav>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

async fn answer(
    dav: Arc<Mutex<Dav>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let expected = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("juniper:{PASSWORD}"))
    );
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
    let path = uri.path().to_string();
    let mut dav = dav.lock().unwrap();
    dav.seen.push(Seen {
        method: method.to_string(),
        path: path.clone(),
        depth: header("depth"),
        if_none_match: header("if-none-match"),
        body: String::from_utf8_lossy(&body).into_owned(),
    });
    if header("authorization").as_deref() != Some(expected.as_str()) {
        return (StatusCode::UNAUTHORIZED, [("WWW-Authenticate", "Basic realm=\"dav\"")])
            .into_response();
    }
    match method.as_str() {
        "REPORT" => {
            let recorded = dav.recorded.get(&path).cloned().unwrap_or_else(|| {
                r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:cal="urn:ietf:params:xml:ns:caldav"></d:multistatus>"#.to_string()
            });
            let (d, c, close) = if recorded.contains("<D:multistatus") {
                ("D", "C", "</D:multistatus>")
            } else {
                ("d", "cal", "</d:multistatus>")
            };
            let mut extra = String::new();
            if !dav.hide_stored {
                for (resource, text) in dav.stored.iter().filter(|(r, _)| r.starts_with(&path)) {
                    extra.push_str(&format!(
                        "<{d}:response><{d}:href>{resource}</{d}:href><{d}:propstat><{d}:prop><{c}:calendar-data>{}</{c}:calendar-data></{d}:prop><{d}:status>HTTP/1.1 200 OK</{d}:status></{d}:propstat></{d}:response>",
                        xml_escape(text)
                    ));
                }
            }
            let body = recorded.replace(close, &format!("{extra}{close}"));
            (StatusCode::MULTI_STATUS, [("Content-Type", "application/xml; charset=utf-8")], body)
                .into_response()
        }
        "PUT" => {
            if header("if-none-match").as_deref() == Some("*") && dav.stored.contains_key(&path) {
                return StatusCode::PRECONDITION_FAILED.into_response();
            }
            dav.stored.insert(path, String::from_utf8_lossy(&body).into_owned());
            StatusCode::CREATED.into_response()
        }
        "GET" => match dav.stored.get(&path) {
            Some(text) => {
                (StatusCode::OK, [("Content-Type", "text/calendar")], text.clone()).into_response()
            }
            None => StatusCode::NOT_FOUND.into_response(),
        },
        _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
    }
}

async fn server() -> Server {
    let dav = Arc::new(Mutex::new(Dav::default()));
    {
        let mut state = dav.lock().unwrap();
        state.recorded.insert(SLOTS.into(), fixture("availability.xml"));
        state.recorded.insert(PERSONAL.into(), fixture("owner-busy.xml"));
    }
    let shared = dav.clone();
    let app = axum::Router::new().fallback(
        move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
            answer(shared.clone(), method, uri, headers, body)
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Server { addr, dav, task }
}

fn auth(password: &str) -> CalDavAuth {
    CalDavAuth::Basic { username: "juniper".into(), password: CredentialSecret::new(password) }
}

impl Server {
    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    /// Slots in the open-slots collection, booked into the owner's own calendar.
    fn calendar(&self) -> CalDavCalendar {
        CalDavCalendar::new(&self.url(SLOTS), auth(PASSWORD))
            .unwrap()
            .with_bookings(&self.url(PERSONAL))
            .unwrap()
            .with_price(1_500)
    }

    fn seen(&self) -> Vec<Seen> {
        self.dav.lock().unwrap().seen.clone()
    }
}

fn window() -> (chrono::DateTime<Utc>, chrono::DateTime<Utc>) {
    (
        Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap(),
        Utc.with_ymd_and_hms(2026, 11, 15, 0, 0, 0).unwrap(),
    )
}

fn ids(slots: &[adk_website::edge::handlers::Slot]) -> Vec<&str> {
    slots.iter().map(|slot| slot.id.as_str()).collect()
}

/// Each occurrence of a recurring slot is its own slot; a cancelled slot and one the owner's
/// own appointment overlaps are not open; a transparent event blocks nothing; prices come
/// from `X-ADK-PRICE`, else the calendar's.
#[tokio::test]
async fn open_slots_are_the_owners_minus_what_is_busy() {
    let server = server().await;
    let calendar = server.calendar();
    let (from, to) = window();
    let slots = calendar.open(from, to).await.unwrap();
    assert_eq!(
        ids(&slots),
        ["tasting-20261103@juniper.example", "consult@juniper.example#20261104T090000Z"],
        "the consultation on the 11th meets the dentist (09:30 UTC in Nairobi time): {slots:#?}"
    );
    assert_eq!((slots[0].minutes, slots[0].price), (30, Some(5_000)));
    assert_eq!((slots[1].minutes, slots[1].price), (60, Some(1_500)));
    assert_eq!(slots[1].starts_at, Utc.with_ymd_and_hms(2026, 11, 4, 9, 0, 0).unwrap());
    assert_eq!(calendar.price("tasting-20261103@juniper.example"), Some(5_000));

    let reports: Vec<Seen> = server.seen().into_iter().filter(|s| s.method == "REPORT").collect();
    assert_eq!(reports.len(), 2, "one listing of each collection");
    for report in &reports {
        assert_eq!(report.depth.as_deref(), Some("1"));
        assert!(
            report.body.contains(r#"<C:expand start="20261101T000000Z" end="20261115T000000Z"/>"#),
            "{}",
            report.body
        );
        assert!(
            report
                .body
                .contains(r#"<C:time-range start="20261101T000000Z" end="20261115T000000Z"/>"#)
        );
    }
}

/// A slot is booked once: the booking is written with `If-None-Match: *` under a name of its
/// own, a retry with the same key answers `true`, another key `false`, and the slot is no
/// longer open. The event names the slot and the key and carries no contact details.
#[tokio::test]
async fn a_slot_is_booked_once() {
    let server = server().await;
    let calendar = server.calendar();
    let (from, to) = window();
    let tasting = "tasting-20261103@juniper.example";
    assert!(calendar.take(tasting, "b-1").await.unwrap());
    assert!(calendar.take(tasting, "b-1").await.unwrap(), "the same booking again");
    assert!(!calendar.take(tasting, "b-2").await.unwrap(), "another booking of a taken slot");
    assert_eq!(
        ids(&calendar.open(from, to).await.unwrap()),
        ["consult@juniper.example#20261104T090000Z"]
    );

    let puts: Vec<Seen> = server.seen().into_iter().filter(|s| s.method == "PUT").collect();
    assert_eq!(puts.len(), 1, "one write: {puts:#?}");
    assert_eq!(puts[0].if_none_match.as_deref(), Some("*"));
    assert!(
        puts[0].path.starts_with(PERSONAL) && puts[0].path.ends_with(".ics"),
        "{}",
        puts[0].path
    );
    let event = &puts[0].body;
    for line in [
        "DTSTART:20261103T070000Z",
        "DTEND:20261103T073000Z",
        "SUMMARY:Booked: Bread tasting",
        "TRANSP:OPAQUE",
        "STATUS:CONFIRMED",
        "X-ADK-SLOT:tasting-20261103@juniper.example",
        "X-ADK-BOOKING-KEY:b-1",
    ] {
        assert!(event.contains(line), "{line} in {event}");
    }
    assert!(!event.contains("ATTENDEE") && !event.contains("mailto:"), "no contact details");

    // The owner's own appointment blocks a slot; an unknown slot is not open.
    assert!(!calendar.take("consult@juniper.example#20261111T090000Z", "b-3").await.unwrap());
    assert!(!calendar.take("no-such-slot@juniper.example", "b-4").await.unwrap());
}

/// Two bookings of one slot that both read it open race to the server: the conditional
/// write lets one through, and the other reads the booking back and is refused.
#[tokio::test]
async fn a_race_for_one_slot_books_it_once() {
    let server = server().await;
    server.dav.lock().unwrap().hide_stored = true;
    let consult = "consult@juniper.example#20261104T090000Z";
    let first = server.calendar();
    assert!(first.take(consult, "b-1").await.unwrap());
    let second = server.calendar();
    assert!(!second.take(consult, "b-2").await.unwrap(), "the conditional write refuses it");
    assert!(second.take(consult, "b-1").await.unwrap(), "the first booking, retried elsewhere");
    let seen = server.seen();
    let puts = seen.iter().filter(|s| s.method == "PUT").count();
    let gets = seen.iter().filter(|s| s.method == "GET").count();
    assert_eq!((puts, gets), (3, 2), "two refused writes, each read back: {seen:#?}");
}

/// With one collection, the slots and their bookings live together: a booking event is not
/// a slot, and the slot it books is no longer open.
#[tokio::test]
async fn slots_and_bookings_share_one_collection() {
    let server = server().await;
    let calendar = CalDavCalendar::new(&server.url(SLOTS), auth(PASSWORD)).unwrap();
    let (from, to) = window();
    let open = calendar.open(from, to).await.unwrap();
    assert_eq!(open.len(), 3, "no owner's calendar blocks the 11th here: {open:#?}");
    assert_eq!(open[1].price, None, "free without a price of its own or the calendar's");
    assert!(calendar.take("consult@juniper.example#20261111T090000Z", "b-1").await.unwrap());
    let put = server.seen().into_iter().find(|s| s.method == "PUT").unwrap();
    assert!(put.path.starts_with(SLOTS), "{}", put.path);
    let open = calendar.open(from, to).await.unwrap();
    assert_eq!(
        ids(&open),
        ["tasting-20261103@juniper.example", "consult@juniper.example#20261104T090000Z"]
    );
}

/// A refused password is an `Unauthorized` error that says what to do; an address that would
/// send it in the clear is refused before any request.
#[tokio::test]
async fn credentials_are_refused_or_never_sent_in_the_clear() {
    let server = server().await;
    let calendar = CalDavCalendar::new(&server.url(SLOTS), auth("wrong")).unwrap();
    let (from, to) = window();
    let error = calendar.open(from, to).await.unwrap_err();
    assert_eq!(error.category, ErrorCategory::Unauthorized, "{error}");
    assert!(error.message.contains("app password"), "{error}");
    assert!(CalDavCalendar::new("http://dav.example.com/calendars/juniper/", auth("x")).is_err());
    assert!(
        format!("{:?}", auth(PASSWORD)).contains("juniper")
            && !format!("{:?}", auth(PASSWORD)).contains(PASSWORD)
    );
}

/// The agent edge books through the CalDAV calendar: `check_availability` lists its open
/// slots, `book` takes one, and a second booking of it is `inventory_unavailable`.
#[tokio::test]
async fn the_edge_books_through_the_owners_calendar() {
    use adk_site::pipeline::{Change, EntryPoint, PipelineOutcome};
    use adk_website::kinds::CharterInputs;
    use adk_website::onboarding::build::SitePlan;
    use adk_website::team::AgentRole;
    use common::edge::{Shop, edge_shop};
    use serde_json::json;

    let server = server().await;
    let site = common::Site::new(
        awp_types::SiteKind::Business,
        "Juniper Bakery",
        "juniper.example",
        &["en"],
    )
    .await;
    let inputs = CharterInputs::new(
        site.site.id,
        "Juniper Bakery",
        common::OWNER,
        "Fresh bread every morning.",
        vec!["en".parse().unwrap()],
        "KES",
    );
    let charter = site.template.draft_charter(&site.team, &inputs);
    site.commit(&charter, &[(awp_types::Role::Owner, common::OWNER)]).await;
    let items = SitePlan::new(&charter, "Juniper Bakery").items(&charter, &[], &["KE".to_string()]);
    let change = Change {
        items,
        media: Vec::new(),
        entry: EntryPoint::SiteAgent { agent: AgentRole::Steward.principal(&site.site) },
        proposer: AgentRole::Steward.principal(&site.site),
        job: "onboarding".into(),
        action_class: "publish".into(),
    };
    let outcome = site.runtime.pipeline().run(change).await.unwrap();
    assert!(matches!(outcome, PipelineOutcome::Published(_)), "release 1: {outcome:?}");
    let edge = edge_shop(
        &site,
        "juniper.example",
        Shop { calendar: Some(Arc::new(server.calendar())), ..Shop::default() },
    )
    .await;

    let window = json!({ "from": "2026-11-01", "to": "2026-11-15" });
    let (status, _, open) =
        edge.send(edge.signed(edge.call("check_availability", "a-1", None, &window))).await;
    assert_eq!(status, StatusCode::OK, "{open}");
    let listed: Vec<&str> =
        open["slots"].as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert_eq!(
        listed,
        ["tasting-20261103@juniper.example", "consult@juniper.example#20261104T090000Z"]
    );

    let contact = edge.consent("contact").await;
    let book = |key: &str| {
        let body = json!({ "slot": "tasting-20261103@juniper.example", "name": "Wanjiru", "email": "wanjiru@mail.example" });
        edge.signed(edge.call("book", key, Some(&contact), &body))
    };
    let (status, _, booked) = edge.send(book("b-1")).await;
    assert_eq!((status, &booked["booked"]), (StatusCode::OK, &json!(true)), "{booked}");
    let (_, _, again) = edge.send(book("b-2")).await;
    assert_eq!(again["error"]["code"], "inventory_unavailable", "{again}");
    let puts = server.seen().into_iter().filter(|s| s.method == "PUT").count();
    assert_eq!(puts, 1, "one booking in the owner's calendar");
}

/// A served site reads its CalDAV calendar from `caldav.json`, its secrets by reference only
/// (A-I19).
#[tokio::test]
async fn caldav_json_configures_the_served_calendar() {
    use adk_website::deploy::project::{ProjectPaths, caldav};

    let server = server().await;
    let dir = tempfile::tempdir().unwrap();
    let paths = ProjectPaths::new(dir.path());
    assert!(caldav(&paths).unwrap().is_none(), "no file, no calendar");
    std::fs::write(dir.path().join("caldav.secret"), format!("{PASSWORD}\n")).unwrap();
    let config = serde_json::json!({
        "slots": server.url(SLOTS),
        "bookings": server.url(PERSONAL),
        "username": "juniper",
        "password": "file:caldav.secret",
        "price": 1500,
        "timeZone": "Africa/Nairobi",
    });
    std::fs::write(dir.path().join("caldav.json"), config.to_string()).unwrap();
    let calendar = caldav(&paths).unwrap().expect("the configured calendar");
    let (from, to) = window();
    assert_eq!(calendar.open(from, to).await.unwrap().len(), 2);

    let mut literal = config.clone();
    literal["password"] = serde_json::json!(PASSWORD);
    std::fs::write(dir.path().join("caldav.json"), literal.to_string()).unwrap();
    let error = caldav(&paths).unwrap_err();
    assert!(error.message.contains("env:VAR"), "{error}");
    let mut zone = config.clone();
    zone["timeZone"] = serde_json::json!("Mars/Olympus");
    std::fs::write(dir.path().join("caldav.json"), zone.to_string()).unwrap();
    assert!(caldav(&paths).is_err());
}

/// A live run against a real CalDAV server: makes a calendar, lists and books its slots,
/// and removes it. With Radicale:
///
/// ```sh
/// pipx install radicale
/// radicale --server-hosts 127.0.0.1:5232 --auth-type none --rights-type owner_only \
///   --storage-filesystem-folder /tmp/radicale &
/// ADK_WEBSITE_CALDAV_HOME=http://127.0.0.1:5232/juniper/ ADK_WEBSITE_CALDAV_USER=juniper \
///   cargo nextest run -p adk-website --test caldav --run-ignored only live_caldav
/// ```
///
/// The nightly `site-hosts` job runs it so.
///
/// `ADK_WEBSITE_CALDAV_USER` and `ADK_WEBSITE_CALDAV_PASSWORD` sign in where the server asks.
#[tokio::test]
#[ignore = "needs a CalDAV server: set ADK_WEBSITE_CALDAV_HOME to a collection the test may make calendars under"]
async fn live_caldav_lists_and_books_once() {
    let home = std::env::var("ADK_WEBSITE_CALDAV_HOME").expect("set ADK_WEBSITE_CALDAV_HOME");
    let home = if home.ends_with('/') { home } else { format!("{home}/") };
    let user = std::env::var("ADK_WEBSITE_CALDAV_USER").ok();
    let password = std::env::var("ADK_WEBSITE_CALDAV_PASSWORD").unwrap_or_default();
    let client = reqwest::Client::new();
    let signed = |request: reqwest::RequestBuilder| match &user {
        Some(user) => request.basic_auth(user, Some(&password)),
        None => request,
    };
    let run = Utc::now().timestamp_millis();
    // A server that makes no home of its own (Radicale without accounts) needs it made first;
    // elsewhere the home exists and this is refused.
    let _ = signed(client.request(Method::from_bytes(b"MKCOL").unwrap(), &home)).send().await;
    let collection = format!("{home}adk-live-{run}/");
    let made = signed(client.request(Method::from_bytes(b"MKCALENDAR").unwrap(), &collection))
        .send()
        .await
        .unwrap();
    assert!(made.status().is_success(), "MKCALENDAR {collection}: {}", made.status());

    let start = Utc::now() + chrono::Duration::days(3);
    let day = start.format("%Y%m%d").to_string();
    for (name, body) in [
        (
            "single.ics",
            format!(
                "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//adk-rust//test//EN\r\nBEGIN:VEVENT\r\nUID:single-{run}\r\nDTSTAMP:{day}T000000Z\r\nDTSTART:{day}T090000Z\r\nDTEND:{day}T093000Z\r\nSUMMARY:Tasting\r\nX-ADK-PRICE:5000\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
            ),
        ),
        (
            "weekly.ics",
            format!(
                "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//adk-rust//test//EN\r\nBEGIN:VEVENT\r\nUID:weekly-{run}\r\nDTSTAMP:{day}T000000Z\r\nDTSTART:{day}T120000Z\r\nDURATION:PT1H\r\nRRULE:FREQ=WEEKLY;COUNT=3\r\nSUMMARY:Consultation\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
            ),
        ),
    ] {
        let put = signed(
            client
                .put(format!("{collection}{name}"))
                .header("Content-Type", "text/calendar")
                .body(body),
        )
        .send()
        .await
        .unwrap();
        assert!(put.status().is_success(), "PUT {name}: {}", put.status());
    }

    let auth = match &user {
        Some(user) => CalDavAuth::Basic {
            username: user.clone(),
            password: CredentialSecret::new(password.clone()),
        },
        None => CalDavAuth::None,
    };
    let calendar = CalDavCalendar::new(&collection, auth).unwrap();
    let from = start - chrono::Duration::days(1);
    let to = start + chrono::Duration::days(30);
    let slots = calendar.open(from, to).await.unwrap();
    assert_eq!(slots.len(), 4, "one slot and three weekly occurrences: {slots:#?}");
    let single = slots.iter().find(|s| s.id == format!("single-{run}")).expect("the single slot");
    assert_eq!((single.minutes, single.price), (30, Some(5_000)));
    let second = slots.iter().filter(|s| s.id.starts_with("weekly-")).nth(1).unwrap().id.clone();
    assert!(calendar.take(&second, "live-1").await.unwrap());
    assert!(calendar.take(&second, "live-1").await.unwrap());
    assert!(!calendar.take(&second, "live-2").await.unwrap());
    let after = calendar.open(from, to).await.unwrap();
    assert_eq!(after.len(), 3, "the booked occurrence is gone: {after:#?}");
    assert!(after.iter().all(|s| s.id != second));

    let _ = signed(client.delete(&collection)).send().await;
}
