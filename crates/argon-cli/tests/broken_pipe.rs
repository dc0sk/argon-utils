// SPDX-License-Identifier: GPL-3.0-or-later
//! `argonctl doctor | head`: when the reader has gone, the program ends quietly with 141 (as if by
//! SIGPIPE) instead of panicking.

use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};

#[test]
fn a_closed_stdout_ends_it_quietly() {
    // The other end is closed before the program starts, so its first write finds no reader --
    // the same "Broken pipe" a pipe gives, without racing the program's first write.
    let (writer, reader) = UnixStream::pair().unwrap();
    drop(reader);
    let out = Command::new(env!("CARGO_BIN_EXE_argonctl"))
        .arg("doctor")
        .stdout(Stdio::from(OwnedFd::from(writer)))
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("panicked"), "it panicked: {stderr}");
    assert_eq!(out.status.code(), Some(141), "stderr: {stderr}");
}
