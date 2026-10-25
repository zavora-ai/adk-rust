//! The line-oriented connection IMAP and SMTP share: plain on loopback, TLS, or upgraded
//! with STARTTLS (RFC 3207, RFC 9051 §6.2.1).

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use rustls::RootCertStore;
use rustls::pki_types::{CertificateDer, ServerName};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use crate::error::{DriverError, Result};

/// The longest line a server may send.
const MAX_LINE: usize = 64 * 1024;
/// The longest literal or message a server may send.
pub(crate) const MAX_LITERAL: usize = 25 * 1024 * 1024;

enum Stream {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
    Closed,
}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_read(cx, buf),
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_read(cx, buf),
            Self::Closed => Poll::Ready(Ok(())),
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_write(cx, buf),
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_write(cx, buf),
            Self::Closed => Poll::Ready(Err(std::io::ErrorKind::NotConnected.into())),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_flush(cx),
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_flush(cx),
            Self::Closed => Poll::Ready(Ok(())),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_shutdown(cx),
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_shutdown(cx),
            Self::Closed => Poll::Ready(Ok(())),
        }
    }
}

/// The TLS client settings: the system's roots, or the roots a test gives.
#[derive(Clone)]
pub(crate) struct Tls {
    connector: TlsConnector,
}

impl Tls {
    /// TLS trusting the system's roots, and `extra` besides.
    pub(crate) fn new(extra: &[CertificateDer<'static>]) -> Result<Self> {
        adk_core::ensure_crypto_provider();
        let mut roots = RootCertStore::empty();
        if extra.is_empty() {
            let native = rustls_native_certs::load_native_certs();
            for certificate in native.certs {
                let _ = roots.add(certificate);
            }
        }
        for certificate in extra {
            roots.add(certificate.clone()).map_err(|error| {
                DriverError::invalid(format!("a root certificate is not valid: {error}"))
            })?;
        }
        let config =
            rustls::ClientConfig::builder().with_root_certificates(roots).with_no_client_auth();
        Ok(Self { connector: TlsConnector::from(Arc::new(config)) })
    }
}

/// A connection to a mail server, read line by line.
pub(crate) struct MailConn {
    stream: Stream,
    buffer: Vec<u8>,
    host: String,
    service: &'static str,
    timeout: Duration,
}

impl MailConn {
    /// Connects to `host:port`, under TLS from the start when `tls` is given.
    pub(crate) async fn connect(
        service: &'static str,
        host: &str,
        port: u16,
        tls: Option<&Tls>,
        timeout: Duration,
    ) -> Result<Self> {
        let tcp = tokio::time::timeout(timeout, TcpStream::connect((host, port)))
            .await
            .map_err(|_| {
                DriverError::unavailable(service, format!("{host}:{port} did not answer in time"))
            })?
            .map_err(|error| {
                DriverError::unavailable(service, format!("{host}:{port}: {error}"))
            })?;
        let mut conn = Self {
            stream: Stream::Plain(tcp),
            buffer: Vec::new(),
            host: host.to_string(),
            service,
            timeout,
        };
        if let Some(tls) = tls {
            conn.upgrade(tls).await?;
        }
        Ok(conn)
    }

    /// Starts TLS on the connection (implicit TLS, or after STARTTLS).
    pub(crate) async fn upgrade(&mut self, tls: &Tls) -> Result<()> {
        if !self.buffer.is_empty() {
            return Err(DriverError::protocol(
                self.service,
                "the server sent data before TLS started",
            ));
        }
        let Stream::Plain(tcp) = std::mem::replace(&mut self.stream, Stream::Closed) else {
            return Err(DriverError::protocol(self.service, "TLS is already on"));
        };
        let name = ServerName::try_from(self.host.clone()).map_err(|_| {
            DriverError::invalid(format!("`{}` is not a TLS server name", self.host))
        })?;
        let stream = tokio::time::timeout(self.timeout, tls.connector.connect(name, tcp))
            .await
            .map_err(|_| DriverError::unavailable(self.service, "the TLS handshake timed out"))?
            .map_err(|error| DriverError::unavailable(self.service, format!("TLS: {error}")))?;
        self.stream = Stream::Tls(Box::new(stream));
        Ok(())
    }

    /// Whether TLS is on.
    pub(crate) fn is_tls(&self) -> bool {
        matches!(self.stream, Stream::Tls(_))
    }

    async fn fill(&mut self) -> Result<()> {
        let mut chunk = [0u8; 8192];
        let read = tokio::time::timeout(self.timeout, self.stream.read(&mut chunk))
            .await
            .map_err(|_| {
                DriverError::unavailable(self.service, "the server did not answer in time")
            })?
            .map_err(|error| DriverError::unavailable(self.service, error))?;
        if read == 0 {
            return Err(DriverError::unavailable(self.service, "the server closed the connection"));
        }
        self.buffer.extend_from_slice(&chunk[..read]);
        Ok(())
    }

    /// The next line, without its line end.
    pub(crate) async fn read_line(&mut self) -> Result<String> {
        loop {
            if let Some(end) = self.buffer.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = self.buffer.drain(..=end).collect();
                let line = String::from_utf8_lossy(&line);
                return Ok(line.trim_end_matches(['\r', '\n']).to_string());
            }
            if self.buffer.len() > MAX_LINE {
                return Err(DriverError::protocol(self.service, "a line is longer than 64 KiB"));
            }
            self.fill().await?;
        }
    }

    /// The next `length` bytes.
    pub(crate) async fn read_bytes(&mut self, length: usize) -> Result<Vec<u8>> {
        if length > MAX_LITERAL {
            return Err(DriverError::protocol(self.service, "a message is larger than 25 MiB"));
        }
        while self.buffer.len() < length {
            self.fill().await?;
        }
        Ok(self.buffer.drain(..length).collect())
    }

    /// Writes `bytes`.
    pub(crate) async fn write(&mut self, bytes: &[u8]) -> Result<()> {
        let write = async {
            self.stream.write_all(bytes).await?;
            self.stream.flush().await
        };
        tokio::time::timeout(self.timeout, write)
            .await
            .map_err(|_| DriverError::unavailable(self.service, "the write timed out"))?
            .map_err(|error| DriverError::unavailable(self.service, error))
    }

    /// Writes `line` and a CRLF.
    pub(crate) async fn write_line(&mut self, line: &str) -> Result<()> {
        let mut bytes = Vec::with_capacity(line.len() + 2);
        bytes.extend_from_slice(line.as_bytes());
        bytes.extend_from_slice(b"\r\n");
        self.write(&bytes).await
    }
}
