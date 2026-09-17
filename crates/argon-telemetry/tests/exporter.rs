// SPDX-License-Identifier: GPL-3.0-or-later
//! Exposition format correctness, and the server's behaviour on odd requests.

use argon_telemetry::{Server, Snapshot, render};
use std::io::{BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};

fn sample() -> Snapshot {
    Snapshot {
        version: "0.1.0",
        mode: "read-only",
        cpu_decicelsius: Some(496),
        fan_pwm: Some(75),
        fan_rpm: Some(2948),
        fan_backend: Some("kernel-pwm"),
        fan_writable: Some(false),
        governor_state: Some((1, 4)),
        vendor_units: vec![
            ("argononed.service".to_owned(), true),
            ("argoneond.service".to_owned(), false),
        ],
        devices: vec![("ups", true), ("zigbee", true), ("oled", true)],
        sensor_failures: 0,
    }
}

/// Every metric line must be preceded by a HELP and a TYPE for its family.
fn assert_well_formed(text: &str) {
    let mut declared = std::collections::HashSet::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# TYPE ") {
            declared.insert(rest.split_whitespace().next().unwrap_or("").to_owned());
            continue;
        }
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let name = line
            .split(['{', ' '])
            .next()
            .expect("a sample line must start with a metric name");
        assert!(
            declared.contains(name),
            "sample for {name} has no # TYPE: {line}"
        );

        let value = line.rsplit(' ').next().unwrap_or("");
        assert!(
            value.parse::<f64>().is_ok(),
            "value {value:?} is not a number, in line: {line}"
        );
    }
}

#[test]
fn the_output_is_well_formed_exposition_format() {
    assert_well_formed(&render(&sample()));
}

#[test]
fn an_empty_snapshot_is_still_well_formed() {
    // The state at startup, before any reading has succeeded. Emitting a malformed scrape
    // there would break the dashboard exactly when something is already wrong.
    assert_well_formed(&render(&Snapshot::default()));
}

#[test]
fn readings_appear_with_their_values() {
    let text = render(&sample());
    assert!(
        text.contains("argon_cpu_temperature_celsius 49.6"),
        "{text}"
    );
    assert!(text.contains("argon_fan_speed_rpm 2948"), "{text}");
    assert!(text.contains("argon_thermal_governor_state 1"), "{text}");
    assert!(
        text.contains("argon_thermal_governor_max_state 4"),
        "{text}"
    );
}

#[test]
fn pwm_is_exported_as_a_ratio_not_a_kernel_scale() {
    // Prometheus convention, and dashboards should not have to know that the kernel counts
    // to 255.
    let text = render(&sample());
    assert!(text.contains("argon_fan_pwm_ratio 0.294"), "{text}");
    assert!(
        !text.contains("argon_fan_pwm_ratio 75"),
        "raw kernel scale leaked out"
    );
}

#[test]
fn absent_readings_are_absent_rather_than_zero() {
    // The distinction this whole design rests on. A zero is indistinguishable from a real
    // reading of zero; a missing metric is a scrape gap, which Prometheus already models.
    let text = render(&Snapshot {
        cpu_decicelsius: None,
        ..sample()
    });
    assert!(
        !text.contains("argon_cpu_temperature_celsius"),
        "an unavailable temperature was exported anyway: {text}"
    );
}

#[test]
fn there_are_no_battery_metrics_yet() {
    // Guards a temptation rather than a bug. Nothing can measure the battery today: the HID
    // interface is dormant and the serial port belongs to the vendor daemon. A plausible-
    // looking zero on a dashboard is worse than a gap, because an alert could fire on it.
    let text = render(&sample());
    for forbidden in ["battery", "charge", "ups_"] {
        assert!(
            !text.contains(forbidden),
            "found {forbidden:?} in the output; nothing can measure that yet: {text}"
        );
    }
}

#[test]
fn label_values_are_escaped() {
    // An unescaped quote would produce a line that parses as something else entirely -- the
    // kind of bug that only appears on the one machine with an odd device name.
    let text = render(&Snapshot {
        vendor_units: vec![(r#"we"ird\unit"#.to_owned(), true)],
        ..Snapshot::default()
    });
    assert!(text.contains(r#"unit="we\"ird\\unit""#), "{text}");
}

#[test]
fn negative_temperatures_render_correctly() {
    // A Pi in a cold shed. The tenths arithmetic must not produce "-4.-5".
    let text = render(&Snapshot {
        cpu_decicelsius: Some(-45),
        ..Snapshot::default()
    });
    assert!(
        text.contains("argon_cpu_temperature_celsius -4.5"),
        "{text}"
    );
}

// ---------------------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------------------

/// Starts a server on an ephemeral port and returns its address.
fn serve_once() -> SocketAddr {
    let server = Server::bind("127.0.0.1:0".parse().unwrap()).expect("bind");
    let addr = server.local_addr().expect("addr");
    std::thread::spawn(move || {
        let _ = server.serve_one(sample);
    });
    addr
}

fn request(addr: SocketAddr, line: &str) -> String {
    let mut s = TcpStream::connect(addr).expect("connect");
    s.write_all(line.as_bytes()).expect("write");
    s.flush().expect("flush");
    let mut body = String::new();
    s.read_to_string(&mut body).expect("read");
    body
}

#[test]
fn a_scrape_returns_metrics() {
    let addr = serve_once();
    let response = request(addr, "GET /metrics HTTP/1.1\r\nHost: x\r\n\r\n");
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    assert!(response.contains("text/plain; version=0.0.4"), "{response}");
    assert!(
        response.contains("argon_cpu_temperature_celsius"),
        "{response}"
    );
}

#[test]
fn an_unknown_path_is_a_404_not_a_crash() {
    let addr = serve_once();
    let response = request(addr, "GET /wat HTTP/1.1\r\n\r\n");
    assert!(response.starts_with("HTTP/1.1 404"), "{response}");
}

#[test]
fn a_post_is_refused() {
    let addr = serve_once();
    let response = request(addr, "POST /metrics HTTP/1.1\r\n\r\n");
    assert!(response.starts_with("HTTP/1.1 405"), "{response}");
}

#[test]
fn garbage_does_not_crash_the_server() {
    let addr = serve_once();
    let response = request(addr, "\x00\x01\x02 not http at all\r\n\r\n");
    // Any well-formed HTTP answer is fine; hanging or panicking is not.
    assert!(response.starts_with("HTTP/1.1"), "{response}");
}

#[test]
fn the_content_length_matches_the_body() {
    // A mismatch makes a scraper hang waiting for bytes that never come.
    let addr = serve_once();
    let response = request(addr, "GET /metrics HTTP/1.1\r\n\r\n");
    let (head, body) = response.split_once("\r\n\r\n").expect("headers and body");
    let declared: usize = head
        .lines()
        .find_map(|l| l.strip_prefix("Content-Length: "))
        .expect("a Content-Length header")
        .trim()
        .parse()
        .expect("a numeric Content-Length");
    assert_eq!(
        declared,
        body.len(),
        "Content-Length disagrees with the body"
    );
}

#[test]
fn a_client_that_says_nothing_does_not_hold_the_server_forever() {
    // Without a read timeout, one silent connection wedges a single-threaded loop.
    let server = Server::bind("127.0.0.1:0".parse().unwrap()).expect("bind");
    let addr = server.local_addr().expect("addr");
    let handle = std::thread::spawn(move || server.serve_one(sample));

    let silent = TcpStream::connect(addr).expect("connect");
    let started = std::time::Instant::now();
    // Hold the connection open without sending anything.
    let _reader = BufReader::new(&silent);
    let result = handle.join().expect("the handler thread should finish");
    assert!(result.is_ok());
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "the server waited {:?} on a silent client",
        started.elapsed()
    );
}
