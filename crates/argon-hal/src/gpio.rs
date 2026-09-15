// SPDX-License-Identifier: GPL-3.0-or-later
//! GPIO line watching.
//!
//! Lines are addressed by chip **label** and line **offset**, never by chip number. On this
//! distro `/dev/gpiochip4` is a udev symlink to `gpiochip0`, so a hardcoded number happens to
//! work today and would be silently wrong after any change to that rule.

use crate::{Error, Result};
use std::path::Path;
use std::time::Duration;

/// Which way a line moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    /// Low to high.
    Rising,
    /// High to low.
    Falling,
}

/// A line transition, timestamped by the kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EdgeEvent {
    /// Which direction.
    pub edge: Edge,
    /// Kernel monotonic timestamp in nanoseconds.
    ///
    /// Taken in the interrupt path, so it is not subject to the scheduler latency that makes
    /// userspace `sleep()`-based pulse timing unreliable under load.
    pub timestamp_ns: u64,
}

/// Watches a single line for edges.
pub struct EdgeWatcher {
    request: gpiocdev::Request,
}

impl EdgeWatcher {
    /// Requests a line for both-edge event reporting.
    ///
    /// # Errors
    ///
    /// Fails if the line cannot be requested — most often because another process holds it,
    /// or because the caller lacks access to the GPIO character device.
    pub fn open(chip: &Path, offset: u32, consumer: &str) -> Result<Self> {
        let request = gpiocdev::Request::builder()
            .on_chip(chip)
            .with_line(offset)
            .with_edge_detection(gpiocdev::line::EdgeDetection::BothEdges)
            .with_consumer(consumer)
            .request()
            .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?;
        Ok(Self { request })
    }

    /// Waits for the next edge, up to `timeout`.
    ///
    /// Returns `Ok(None)` on timeout.
    ///
    /// # Errors
    ///
    /// Fails if waiting or reading the event fails.
    pub fn next_edge(&self, timeout: Duration) -> Result<Option<EdgeEvent>> {
        let ready = self
            .request
            .wait_edge_event(timeout)
            .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?;
        if !ready {
            return Ok(None);
        }
        let event = self
            .request
            .read_edge_event()
            .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?;
        Ok(Some(EdgeEvent {
            edge: match event.kind {
                gpiocdev::line::EdgeKind::Rising => Edge::Rising,
                gpiocdev::line::EdgeKind::Falling => Edge::Falling,
            },
            timestamp_ns: event.timestamp_ns,
        }))
    }
}
