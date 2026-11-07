//! `BrowserSession::frames` against a fake WebDriver server whose screenshot the test sets.

use adk_browser::{BrowserConfig, BrowserSession, MIN_FRAME_INTERVAL};
use serde_json::json;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// A WebDriver server that creates sessions and answers screenshots with `page`.
struct WebDriverFixture {
    address: SocketAddr,
    page: Arc<Mutex<String>>,
    sessions: Arc<AtomicUsize>,
    screenshots: Arc<AtomicUsize>,
    stopped: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl WebDriverFixture {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let page = Arc::new(Mutex::new("cGFnZS0x".to_string()));
        let sessions = Arc::new(AtomicUsize::new(0));
        let screenshots = Arc::new(AtomicUsize::new(0));
        let stopped = Arc::new(AtomicBool::new(false));
        let worker = {
            let (page, sessions, screenshots, stopped) =
                (page.clone(), sessions.clone(), screenshots.clone(), stopped.clone());
            std::thread::spawn(move || {
                for connection in listener.incoming() {
                    if stopped.load(Ordering::SeqCst) {
                        break;
                    }
                    let mut connection = connection.unwrap();
                    connection.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                    let mut reader = BufReader::new(&mut connection);
                    let mut request = String::new();
                    reader.read_line(&mut request).unwrap();
                    let mut length = 0;
                    loop {
                        let mut header = String::new();
                        reader.read_line(&mut header).unwrap();
                        if header == "\r\n" || header.is_empty() {
                            break;
                        }
                        if let Some((name, value)) = header.split_once(':')
                            && name.eq_ignore_ascii_case("content-length")
                        {
                            length = value.trim().parse::<usize>().unwrap();
                        }
                    }
                    reader.read_exact(&mut vec![0; length]).unwrap();
                    let path = request.split_whitespace().nth(1).unwrap();
                    let value = if request.starts_with("POST /session ") {
                        let index = sessions.fetch_add(1, Ordering::SeqCst) + 1;
                        json!({"sessionId":format!("session-{index}"), "capabilities":{"browserName":"chrome"}})
                    } else if path.ends_with("/screenshot") {
                        screenshots.fetch_add(1, Ordering::SeqCst);
                        json!(page.lock().unwrap().clone())
                    } else if path.ends_with("/title") {
                        json!("Fixture page")
                    } else if path.ends_with("/window/rect") {
                        json!({"x":0,"y":0,"width":1280,"height":720})
                    } else {
                        serde_json::Value::Null
                    };
                    let body = json!({"value":value}).to_string();
                    write!(
                        connection,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .unwrap();
                }
            })
        };
        Self { address, page, sessions, screenshots, stopped, worker: Some(worker) }
    }

    fn session(&self) -> Arc<BrowserSession> {
        Arc::new(BrowserSession::new(
            BrowserConfig::new().webdriver_url(format!("http://{}", self.address)),
        ))
    }

    fn show(&self, page: &str) {
        *self.page.lock().unwrap() = page.to_string();
    }
}

impl Drop for WebDriverFixture {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

const WAIT: Duration = Duration::from_secs(5);

#[tokio::test]
async fn watching_never_starts_a_browser() {
    let fixture = WebDriverFixture::start();
    let session = fixture.session();

    let feed = session.frames(MIN_FRAME_INTERVAL);
    tokio::time::sleep(MIN_FRAME_INTERVAL * 4).await;

    assert!(feed.latest().is_none());
    assert_eq!(fixture.sessions.load(Ordering::SeqCst), 0, "no WebDriver session was created");
    assert_eq!(fixture.screenshots.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_new_page_is_a_new_frame_and_a_still_page_is_not() {
    let fixture = WebDriverFixture::start();
    let session = fixture.session();
    session.start().await.unwrap();
    let mut feed = session.frames(MIN_FRAME_INTERVAL);

    let first = tokio::time::timeout(WAIT, feed.next()).await.unwrap().unwrap();
    assert_eq!((first.seq, &*first.png_base64), (1, "cGFnZS0x"));

    // The page stays the same for several ticks: screenshots are taken, no frame is published.
    let taken = fixture.screenshots.load(Ordering::SeqCst);
    tokio::time::sleep(MIN_FRAME_INTERVAL * 4).await;
    assert!(fixture.screenshots.load(Ordering::SeqCst) > taken, "the feed kept capturing");
    assert_eq!(feed.latest().unwrap().seq, 1);

    fixture.show("cGFnZS0y");
    let second = tokio::time::timeout(WAIT, feed.next()).await.unwrap().unwrap();
    assert_eq!((second.seq, &*second.png_base64), (2, "cGFnZS0y"));
    assert!(second.taken_at >= first.taken_at);
}

#[tokio::test]
async fn the_feed_ends_with_its_session() {
    let fixture = WebDriverFixture::start();
    let session = fixture.session();
    session.start().await.unwrap();
    let mut feed = session.frames(MIN_FRAME_INTERVAL);
    tokio::time::timeout(WAIT, feed.next()).await.unwrap().unwrap();

    drop(session);

    assert!(tokio::time::timeout(WAIT, feed.next()).await.unwrap().is_none());
}

#[tokio::test]
async fn the_feed_stops_capturing_when_its_last_handle_is_dropped() {
    let fixture = WebDriverFixture::start();
    let session = fixture.session();
    session.start().await.unwrap();
    let mut feed = session.frames(MIN_FRAME_INTERVAL);
    tokio::time::timeout(WAIT, feed.next()).await.unwrap().unwrap();
    let viewer = feed.subscribe();

    drop(feed);
    tokio::time::sleep(MIN_FRAME_INTERVAL).await;
    let taken = fixture.screenshots.load(Ordering::SeqCst);
    tokio::time::sleep(MIN_FRAME_INTERVAL * 4).await;

    assert_eq!(fixture.screenshots.load(Ordering::SeqCst), taken, "no capture after the drop");
    assert_eq!(
        viewer.borrow().as_ref().map(|frame| frame.seq),
        Some(1),
        "viewers keep the last frame"
    );
}
