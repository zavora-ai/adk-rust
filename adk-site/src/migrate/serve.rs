//! A loopback static file server for previews: the parity check's screenshots of a build, and
//! a frozen snapshot served without any upstream (spec 3.5 §7.8 "Freeze", §7.8.1).
//!
//! It answers `GET` and `HEAD` from one directory, honours the JSON redirects file the
//! generator writes (`_redirects.json`), runs no code and contacts nothing.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use awp_types::Redirect;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::error::{MigrateError, Result};
use crate::redirects::JSON_REDIRECTS_FILE;
use crate::release::content_type;

/// Serves a directory on `127.0.0.1` until dropped.
///
/// # Example
///
/// ```rust
/// use adk_site::migrate::serve::StaticServer;
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let dir = tempfile::tempdir().unwrap();
/// std::fs::write(dir.path().join("index.html"), "<p>hi</p>").unwrap();
/// let server = StaticServer::serve(dir.path()).await.unwrap();
/// assert!(server.origin().starts_with("http://127.0.0.1:"));
/// # }
/// ```
#[derive(Debug)]
pub struct StaticServer {
    address: SocketAddr,
    requests: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for StaticServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn percent_decode(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The file under `root` that answers `path`, refusing anything that leaves `root`.
fn resolve(root: &Path, path: &str) -> Option<PathBuf> {
    let decoded = percent_decode(path)?;
    if decoded.split('/').any(|s| s == ".." || s.contains('\\')) {
        return None;
    }
    let relative = decoded.trim_start_matches('/');
    let candidate = if relative.is_empty() || relative.ends_with('/') {
        root.join(relative).join("index.html")
    } else {
        root.join(relative)
    };
    Some(candidate)
}

impl StaticServer {
    /// Serves `root` on a free loopback port.
    ///
    /// # Errors
    ///
    /// [`MigrateError::Service`] when no port can be bound.
    pub async fn serve(root: &Path) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| MigrateError::Service(format!("static server: {e}")))?;
        let address = listener.local_addr().map_err(|e| MigrateError::Service(e.to_string()))?;
        let redirects: BTreeMap<String, Redirect> = std::fs::read(root.join(JSON_REDIRECTS_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Vec<Redirect>>(&bytes).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|r| (r.from.clone(), r))
            .collect();
        let root = Arc::new(root.to_path_buf());
        let redirects = Arc::new(redirects);
        let requests = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&requests);
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let (root, redirects, counter) =
                    (Arc::clone(&root), Arc::clone(&redirects), Arc::clone(&counter));
                tokio::spawn(async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    let _ = answer(stream, &root, &redirects).await;
                });
            }
        });
        Ok(Self { address, requests, task })
    }

    /// `http://127.0.0.1:<port>`.
    pub fn origin(&self) -> String {
        format!("http://{}", self.address)
    }

    /// Requests answered so far.
    pub fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

async fn answer(
    mut stream: TcpStream,
    root: &Path,
    redirects: &BTreeMap<String, Redirect>,
) -> std::io::Result<()> {
    let mut head = Vec::new();
    let mut buffer = [0u8; 4096];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < 16 * 1024 {
        let read = stream.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        head.extend_from_slice(&buffer[..read]);
    }
    let text = String::from_utf8_lossy(&head);
    let mut parts = text.lines().next().unwrap_or_default().split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or("/").to_string();
    let path = target.split(['?', '#']).next().unwrap_or("/").to_string();
    let respond = |status: &str, kind: &str, extra: &str, body: &[u8], head_only: bool| {
        let mut out = format!(
            "HTTP/1.1 {status}\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n{extra}\r\n",
            body.len()
        )
        .into_bytes();
        if !head_only {
            out.extend_from_slice(body);
        }
        out
    };
    let response = if method != "GET" && method != "HEAD" {
        respond("405 Method Not Allowed", "text/plain", "allow: GET, HEAD\r\n", b"", false)
    } else if let Some(rule) = redirects.get(&path) {
        let status = match rule.status {
            301 => "301 Moved Permanently",
            302 => "302 Found",
            307 => "307 Temporary Redirect",
            308 => "308 Permanent Redirect",
            _ => "410 Gone",
        };
        respond(status, "text/plain", &format!("location: {}\r\n", rule.to), b"", method == "HEAD")
    } else {
        match resolve(root, &path) {
            Some(file) if file.is_file() => match tokio::fs::read(&file).await {
                Ok(body) => {
                    respond("200 OK", content_type(&path_name(&file)), "", &body, method == "HEAD")
                }
                Err(_) => respond("500 Internal Server Error", "text/plain", "", b"", false),
            },
            Some(file) if file.is_dir() || root.join(path.trim_start_matches('/')).is_dir() => {
                respond(
                    "301 Moved Permanently",
                    "text/plain",
                    &format!("location: {path}/\r\n"),
                    b"",
                    false,
                )
            }
            _ => {
                let body = tokio::fs::read(root.join("404.html")).await.unwrap_or_default();
                respond("404 Not Found", "text/html; charset=utf-8", "", &body, method == "HEAD")
            }
        }
    };
    stream.write_all(&response).await?;
    stream.shutdown().await
}

fn path_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_never_leave_the_root() {
        let root = Path::new("/srv/site");
        assert_eq!(resolve(root, "/"), Some(root.join("index.html")));
        assert_eq!(resolve(root, "/a%20b/"), Some(root.join("a b/index.html")));
        assert_eq!(resolve(root, "/x.css"), Some(root.join("x.css")));
        assert_eq!(resolve(root, "/../etc/passwd"), None);
        assert_eq!(resolve(root, "/%2e%2e/etc/passwd"), None);
    }
}
