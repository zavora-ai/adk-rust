use adk_browser::{BrowserConfig, BrowserSession};
use serde_json::json;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;

struct WebDriverFixture {
    address: SocketAddr,
    stale: Arc<AtomicBool>,
    sessions: Arc<AtomicUsize>,
    stopped: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl WebDriverFixture {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let stale = Arc::new(AtomicBool::new(false));
        let sessions = Arc::new(AtomicUsize::new(0));
        let stopped = Arc::new(AtomicBool::new(false));
        let worker = {
            let stale = stale.clone();
            let sessions = sessions.clone();
            let stopped = stopped.clone();
            std::thread::spawn(move || {
                for connection in listener.incoming() {
                    if stopped.load(Ordering::SeqCst) {
                        break;
                    }
                    let mut connection = connection.unwrap();
                    connection.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
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
                    let unavailable =
                        path == "/session/session-1/title" && stale.load(Ordering::SeqCst);
                    let value = if unavailable {
                        json!({"error":"invalid session id", "message":"session ended", "stacktrace":""})
                    } else if request.starts_with("POST /session ") {
                        let index = sessions.fetch_add(1, Ordering::SeqCst) + 1;
                        json!({"sessionId":format!("session-{index}"), "capabilities":{"browserName":"chrome"}})
                    } else if path.ends_with("/title") {
                        json!("Fixture page")
                    } else if path.ends_with("/window/rect") {
                        json!({"x":0,"y":0,"width":1280,"height":720})
                    } else {
                        serde_json::Value::Null
                    };
                    let body = json!({"value":value}).to_string();
                    let status = if unavailable { "404 Not Found" } else { "200 OK" };
                    write!(
                        connection,
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    ).unwrap();
                }
            })
        };
        Self { address, stale, sessions, stopped, worker: Some(worker) }
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

#[tokio::test]
async fn explicit_start_recovers_an_unavailable_session() {
    let fixture = WebDriverFixture::start();
    let config = BrowserConfig {
        webdriver_url: format!("http://{}", fixture.address),
        require_explicit_start: true,
        ..Default::default()
    };
    let session = BrowserSession::new(config);
    session.start().await.unwrap();
    fixture.stale.store(true, Ordering::SeqCst);
    assert!(session.ensure_started().await.is_err());
    assert_eq!(fixture.sessions.load(Ordering::SeqCst), 1);

    session.start().await.unwrap();
    assert_eq!(session.title().await.unwrap(), "Fixture page");
    assert_eq!(fixture.sessions.load(Ordering::SeqCst), 2);
    session.start().await.unwrap();
    assert_eq!(fixture.sessions.load(Ordering::SeqCst), 2);
    session.stop().await.unwrap();
}
