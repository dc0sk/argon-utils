// SPDX-License-Identifier: GPL-3.0-or-later
//! A minimal HTTP server for `/metrics`.
//!
//! Hand-rolled rather than pulling in a web framework. A Prometheus scrape is one `GET` with
//! no body, no routing, no state and no TLS; a framework would add a large dependency tree
//! and an async runtime to a daemon whose other job is writing single bytes to an I2C device.

use crate::render::Snapshot;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

/// How long a client may take to send its request line before being dropped.
///
/// Without this, a connection that opens and says nothing holds the handler thread forever,
/// which is a trivial denial of service against a single-threaded loop.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Longest request line accepted, to bound memory against a client that never sends a newline.
const MAX_REQUEST_LINE: u64 = 8 * 1024;

/// Why the exporter could not start or serve.
#[derive(Debug)]
pub enum ServerError {
    /// The listening socket could not be opened.
    Bind(std::io::Error),
    /// Accepting a connection failed.
    Accept(std::io::Error),
}

impl std::fmt::Display for ServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bind(e) => write!(f, "cannot listen: {e}"),
            Self::Accept(e) => write!(f, "cannot accept: {e}"),
        }
    }
}

impl std::error::Error for ServerError {}

/// Serves `/metrics` over HTTP.
pub struct Server {
    listener: TcpListener,
}

impl Server {
    /// Binds to an address.
    ///
    /// Callers should bind to loopback unless they mean otherwise: these metrics describe a
    /// machine's thermal and power state, and the daemon has no authentication of any kind.
    ///
    /// # Errors
    ///
    /// Fails if the address cannot be bound.
    pub fn bind(addr: SocketAddr) -> Result<Self, ServerError> {
        let listener = TcpListener::bind(addr).map_err(ServerError::Bind)?;
        Ok(Self { listener })
    }

    /// The address actually bound, which differs from the request when port 0 was asked for.
    ///
    /// # Errors
    ///
    /// Fails if the socket address cannot be read back.
    pub fn local_addr(&self) -> Result<SocketAddr, ServerError> {
        self.listener.local_addr().map_err(ServerError::Bind)
    }

    /// Serves one request, calling `snapshot` to gather metrics.
    ///
    /// One at a time, deliberately: a scrape is infrequent and cheap, and a thread pool would
    /// let a misconfigured scraper fan out against a device this daemon is trying to keep
    /// single-writer.
    ///
    /// # Errors
    ///
    /// Fails if accepting the connection fails. A malformed request is answered, not an error.
    pub fn serve_one<F: FnOnce() -> Snapshot>(&self, snapshot: F) -> Result<(), ServerError> {
        let (stream, _peer) = self.listener.accept().map_err(ServerError::Accept)?;
        // A failure to answer one client is not a reason to stop exporting.
        let _ = handle(stream, snapshot);
        Ok(())
    }
}

/// Reads one request line and writes the response.
fn handle<F: FnOnce() -> Snapshot>(mut stream: TcpStream, snapshot: F) -> std::io::Result<()> {
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    stream.set_write_timeout(Some(READ_TIMEOUT))?;

    // Bound the read: a client that opens a connection and never sends a newline would
    // otherwise grow this string until the process dies.
    let mut line = String::new();
    let limited = stream.try_clone()?.take(MAX_REQUEST_LINE);
    BufReader::new(limited).read_line(&mut line)?;

    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");

    let (status, content_type, body) = match (method, path) {
        ("GET", "/metrics") => (
            "200 OK",
            "text/plain; version=0.0.4",
            crate::render(&snapshot()),
        ),
        ("GET", "/") => (
            "200 OK",
            "text/plain; charset=utf-8",
            "argon-utils exporter\nmetrics at /metrics\n".to_owned(),
        ),
        ("GET", _) => (
            "404 Not Found",
            "text/plain; charset=utf-8",
            "not found\n".to_owned(),
        ),
        _ => (
            "405 Method Not Allowed",
            "text/plain; charset=utf-8",
            "only GET is supported\n".to_owned(),
        ),
    };

    write!(
        stream,
        "HTTP/1.1 {status}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n{body}",
        body.len()
    )?;
    stream.flush()
}
