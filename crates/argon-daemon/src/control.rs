// SPDX-License-Identifier: GPL-3.0-or-later
//! The control socket: requests from `argonctl`, relayed to the thread that can act on them.
//!
//! Anything that talks to the UPS has to happen on the UPS thread, because it owns the port and
//! CDC-ACM has no arbitration. So this thread only listens, parses, hands each request across a
//! channel, and writes back whatever comes back. It holds no device and makes no decisions.

use argon_device::control::{self, Request, Response};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

/// A request on its way to the UPS thread, with the channel its answer goes back on.
pub type Message = (Request, Sender<Response>);

/// Longest request line accepted. A request is a few dozen bytes; this bounds what a
/// misbehaving client can make the daemon buffer.
const MAX_LINE: u64 = 4_096;

/// How long a client gets to send its request.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the UPS thread gets to answer. It answers between polls, so this covers one poll
/// interval plus the device exchanges a request makes.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(30);

/// Starts listening on `path`.
///
/// Returns `None` -- after saying why -- if the socket cannot be created, which is expected when
/// argond is run by hand outside systemd, where `/run/argon-utils` is not provided. The daemon
/// runs without it: only the requests are lost, never monitoring.
pub fn spawn(
    path: &Path,
    to_ups: Sender<Message>,
    waiting: Arc<AtomicBool>,
    stopping: Arc<AtomicBool>,
) -> Option<JoinHandle<()>> {
    // A socket file left by a previous run would make bind fail.
    let _ = std::fs::remove_file(path);
    let listener = match UnixListener::bind(path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("argond: control: cannot listen on {}: {e}", path.display());
            return None;
        }
    };
    // Owner only. argond runs as `argon`; root passes the permission check regardless. Every
    // request is an action on the machine, so an ordinary login must not be able to send one.
    if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        eprintln!(
            "argond: control: cannot restrict {}: {e}; not listening",
            path.display()
        );
        let _ = std::fs::remove_file(path);
        return None;
    }
    if let Err(e) = listener.set_nonblocking(true) {
        eprintln!("argond: control: {e}; not listening");
        return None;
    }
    eprintln!("argond: control: listening on {}", path.display());

    let path = path.to_path_buf();
    Some(std::thread::spawn(move || {
        serve(&listener, &to_ups, &waiting, &stopping);
        let _ = std::fs::remove_file(&path);
    }))
}

fn serve(
    listener: &UnixListener,
    to_ups: &Sender<Message>,
    waiting: &AtomicBool,
    stopping: &AtomicBool,
) {
    while !stopping.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => handle(stream, to_ups, waiting),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => {
                eprintln!("argond: control: accept failed: {e}");
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

fn handle(stream: UnixStream, to_ups: &Sender<Message>, waiting: &AtomicBool) {
    let response = match read_request(&stream) {
        Ok(req) => relay(req, to_ups, waiting),
        Err(why) => Response::error(why),
    };
    let mut stream = stream;
    if let Ok(line) = control::to_line(&response) {
        let _ = stream.write_all(line.as_bytes());
    }
    // Closing a Unix socket with unread input makes the kernel reset the connection, and the
    // client then sees "connection reset" instead of the answer that says why it was refused.
    // So: finish writing, then read and discard what is left -- bounded, so a client that never
    // stops sending costs at most this much, and at most the read timeout.
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let _ = std::io::copy(&mut (&stream).take(MAX_DRAIN), &mut std::io::sink());
}

/// The most leftover input read and discarded before closing a connection.
const MAX_DRAIN: u64 = 64 * 1_024;

fn read_request(stream: &UnixStream) -> Result<Request, String> {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let mut line = String::new();
    BufReader::new(stream.take(MAX_LINE))
        .read_line(&mut line)
        .map_err(|e| format!("reading the request: {e}"))?;
    if !line.ends_with('\n') {
        return Err("the request was not a single complete line".into());
    }
    control::from_line(&line).map_err(|e| format!("not a request argond understands: {e}"))
}

fn relay(req: Request, to_ups: &Sender<Message>, waiting: &AtomicBool) -> Response {
    let (reply_tx, reply_rx) = mpsc::channel();
    if to_ups.send((req, reply_tx)).is_err() {
        return Response::error("UPS monitoring is not running, so nothing can act on this");
    }
    // Wakes the UPS thread out of its sleep between polls.
    waiting.store(true, Ordering::Relaxed);
    reply_rx.recv_timeout(ANSWER_TIMEOUT).unwrap_or_else(|_| {
        Response::error("the UPS thread did not answer in time; the request may not have run")
    })
}

/// Where the control socket goes.
#[must_use]
pub fn socket_path() -> PathBuf {
    PathBuf::from(control::SOCKET_PATH)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A socket in a scratch directory, with a stand-in for the UPS thread that answers every
    /// request with `answer`.
    fn serve_with(answer: Response) -> (PathBuf, Arc<AtomicBool>, JoinHandle<()>, JoinHandle<()>) {
        let dir = std::env::temp_dir().join(format!("argond-control-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("c-{}.sock", rand_suffix()));
        let (tx, rx) = mpsc::channel::<Message>();
        let stopping = Arc::new(AtomicBool::new(false));
        let waiting = Arc::new(AtomicBool::new(false));
        let server = spawn(&path, tx, Arc::clone(&waiting), Arc::clone(&stopping)).unwrap();
        let ups = std::thread::spawn(move || {
            while let Ok((_, reply)) = rx.recv() {
                let _ = reply.send(answer.clone());
            }
        });
        (path, stopping, server, ups)
    }

    fn rand_suffix() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    fn ask(path: &Path, bytes: &[u8]) -> String {
        let mut s = UnixStream::connect(path).unwrap();
        s.write_all(bytes).unwrap();
        s.shutdown(std::net::Shutdown::Write).unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    }

    #[test]
    fn a_request_is_relayed_and_its_answer_returned() {
        let answer = Response::PoweroffScheduled {
            wake_unix: 120,
            poweroff_unix: 60,
        };
        let (path, stopping, server, _ups) = serve_with(answer.clone());
        let req = control::to_line(&Request::PoweroffWithWake { at_unix: 120 }).unwrap();
        let got: Response = control::from_line(&ask(&path, req.as_bytes())).unwrap();
        assert_eq!(got, answer);
        stopping.store(true, Ordering::Relaxed);
        server.join().unwrap();
        assert!(!path.exists(), "the socket file was left behind");
    }

    #[test]
    fn the_socket_is_owner_only() {
        let (path, stopping, server, _ups) = serve_with(Response::error("unused"));
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "control socket mode {mode:o}");
        stopping.store(true, Ordering::Relaxed);
        server.join().unwrap();
    }

    #[test]
    fn garbage_gets_an_error_and_never_reaches_the_ups_thread() {
        let (path, stopping, server, _ups) = serve_with(Response::PoweroffScheduled {
            wake_unix: 0,
            poweroff_unix: 0,
        });
        for junk in [
            &b"poweroff now\n"[..],
            b"{\"reboot\":{}}\n",
            b"no newline at all",
        ] {
            let got: Response = control::from_line(&ask(&path, junk)).unwrap();
            assert!(
                matches!(got, Response::Error { .. }),
                "{junk:?} was accepted: {got:?}"
            );
        }
        stopping.store(true, Ordering::Relaxed);
        server.join().unwrap();
    }

    #[test]
    fn an_oversized_request_is_refused() {
        let (path, stopping, server, _ups) = serve_with(Response::PoweroffScheduled {
            wake_unix: 0,
            poweroff_unix: 0,
        });
        let mut big = vec![b'x'; 10_000];
        big.push(b'\n');
        let got: Response = control::from_line(&ask(&path, &big)).unwrap();
        assert!(matches!(got, Response::Error { .. }));
        stopping.store(true, Ordering::Relaxed);
        server.join().unwrap();
    }
}
