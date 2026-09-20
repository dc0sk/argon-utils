// SPDX-License-Identifier: GPL-3.0-or-later
//! Configuration parsing, and the things it must refuse.

use argon_device::config::{Config, ConfigError};
use argon_hal::mode::Mode;

#[test]
fn the_default_is_safe_and_round_trips() {
    let c = Config::default();
    assert_eq!(
        c.mode().unwrap(),
        Mode::ReadOnly,
        "the default mode must be read-only"
    );
    assert!(
        !c.fan.allow_stop,
        "the default must not permit stopping the fan"
    );

    let round_tripped = Config::from_toml(&c.to_toml().unwrap()).unwrap();
    assert_eq!(round_tripped, c);
}

#[test]
fn an_empty_config_yields_the_defaults() {
    // A missing file should behave like an unconfigured install, not like an error.
    assert_eq!(Config::from_toml("").unwrap(), Config::default());
}

#[test]
fn a_misspelled_key_is_an_error_not_a_silent_default() {
    // The whole reason deny_unknown_fields is set. `allow_stops` would otherwise read as the
    // default and nobody would learn otherwise until the fan was off on a hot machine.
    let err = Config::from_toml("[fan]\nallow_stops = true\n").unwrap_err();
    assert!(matches!(err, ConfigError::Toml(_)), "got {err:?}");
    assert!(
        format!("{err}").contains("allow_stops"),
        "the error should name the offending key: {err}"
    );
}

#[test]
fn a_misspelled_top_level_key_is_also_caught() {
    let err = Config::from_toml("moed = \"full\"\n").unwrap_err();
    assert!(matches!(err, ConfigError::Toml(_)));
}

#[test]
fn an_unknown_mode_is_rejected() {
    let err = Config::from_toml("mode = \"manged\"\n").unwrap_err();
    assert!(
        matches!(err, ConfigError::BadValue { key: "mode", .. }),
        "got {err:?}"
    );
}

#[test]
fn every_valid_mode_parses() {
    for name in ["read-only", "managed", "full"] {
        let c = Config::from_toml(&format!("mode = {name:?}\n")).unwrap();
        assert_eq!(c.mode().unwrap().as_str(), name);
    }
}

#[test]
fn auto_dialect_is_rejected_with_the_reason() {
    // An operator writing `auto` has a specific wrong idea -- that the dialect can be
    // detected -- and the message should correct it rather than just say "invalid".
    let err = Config::from_toml("[mcu]\ndialect = \"auto\"\n").unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("no auto"),
        "message does not explain why: {msg}"
    );
    assert!(
        msg.contains("ADR-0002"),
        "message does not point at the reasoning: {msg}"
    );
}

#[test]
fn the_register_dialect_is_refused_in_a_default_build() {
    let err = Config::from_toml("[mcu]\ndialect = \"register\"\n").unwrap_err();
    assert!(
        matches!(
            err,
            ConfigError::BadValue {
                key: "mcu.dialect",
                ..
            }
        ),
        "got {err:?}"
    );
}

#[test]
fn a_zero_safe_duty_is_refused() {
    // A safety fallback of "stop the fan" is not a safety fallback.
    let err = Config::from_toml("[fan]\nsafe_duty = 0\n").unwrap_err();
    assert!(
        matches!(
            err,
            ConfigError::BadValue {
                key: "fan.safe_duty",
                ..
            }
        ),
        "got {err:?}"
    );
}

#[test]
fn a_zero_poll_interval_is_refused() {
    let err = Config::from_toml("[fan]\npoll_interval_s = 0\n").unwrap_err();
    assert!(matches!(
        err,
        ConfigError::BadValue {
            key: "fan.poll_interval_s",
            ..
        }
    ));
}

#[test]
fn an_out_of_range_duty_is_refused() {
    let err = Config::from_toml("[fan]\ncurve = [ { temp_c = 55, duty = 150 } ]\n").unwrap_err();
    assert!(
        matches!(
            err,
            ConfigError::BadValue {
                key: "fan.curve.duty",
                ..
            }
        ),
        "got {err:?}"
    );
}

#[test]
fn a_curve_that_cools_less_as_it_heats_is_refused() {
    let err = Config::from_toml(
        "[fan]\ncurve = [ { temp_c = 55, duty = 80 }, { temp_c = 65, duty = 40 } ]\n",
    )
    .unwrap_err();
    assert!(matches!(err, ConfigError::Curve(_)), "got {err:?}");
}

#[test]
fn an_unknown_ups_source_is_refused() {
    assert!(Config::from_toml("[ups]\nsource = \"magic\"\n").is_err());
    for good in ["serial", "hid", "none"] {
        assert!(
            Config::from_toml(&format!("[ups]\nsource = {good:?}\n")).is_ok(),
            "{good} should be accepted"
        );
    }
}

#[test]
fn a_realistic_config_parses_and_yields_a_usable_curve() {
    let c = Config::from_toml(
        r#"
mode = "managed"

[fan]
curve = [
    { temp_c = 50, duty = 20 },
    { temp_c = 60, duty = 55 },
    { temp_c = 70, duty = 100 },
]
hysteresis_c = 4
min_duty = 15
safe_duty = 60
allow_stop = false

[mcu]
dialect = "legacy"
bus = "/dev/i2c-1"

[ups]
source = "serial"
port = "/dev/argon-ups"
poll_interval_s = 15
"#,
    )
    .unwrap();

    assert_eq!(c.mode().unwrap(), Mode::Managed);
    let curve = c.fan_curve().unwrap();
    assert_eq!(curve.points().len(), 3);
    assert_eq!(curve.duty_for(650).percent(), 55);
    assert_eq!(c.ups.poll_interval_s, 15);
}

#[test]
fn the_shipped_example_config_is_valid() {
    // A commented example that does not parse is worse than none: it is the first thing an
    // operator copies.
    let example = include_str!("../../../packaging/config/config.toml");
    let c = Config::from_toml(example).expect("the shipped example must parse");
    assert_eq!(
        c.mode().unwrap(),
        Mode::ReadOnly,
        "the example must ship in read-only mode"
    );
}

#[test]
fn battery_policy_thresholds_are_validated() {
    // critical at or above low would make "low" unreachable.
    let err = Config::from_toml("[ups]\nlow_percent = 10\ncritical_percent = 15\n").unwrap_err();
    assert!(
        format!("{err}").contains("critical_percent below low_percent"),
        "{err}"
    );

    // Zero confirmations would act on a single glitched reading.
    let err = Config::from_toml("[ups]\nconfirmations = 0\n").unwrap_err();
    assert!(format!("{err}").contains("single reading"), "{err}");
}

#[test]
fn the_default_ups_settings_match_the_policy_defaults() {
    // Two sources of defaults that must not drift apart.
    assert_eq!(
        Config::default().ups.policy(),
        argon_proto::ups::policy::PolicyConfig::default()
    );
}

#[test]
fn the_t12_test_config_is_valid() {
    // The hardware test's instructions point at this file; a typo there would only show up
    // mid-test, with mains already unplugged.
    let c = Config::from_toml(include_str!("../../../docs/testing/t12-ups-shutdown.toml"))
        .expect("docs/testing/t12-ups-shutdown.toml must parse");
    assert_eq!(c.mode().unwrap(), argon_hal::mode::Mode::Full);
    assert_eq!(c.ups.shutdown_delay_min, 5);
}

#[test]
fn missing_sections_are_named_so_an_old_config_is_visible() {
    // A config kept from an older version through an upgrade: its features run on defaults
    // silently, which is how a disabled OLED looked like a broken tray switch.
    let old = "mode = \"full\"\n[fan]\nenabled = false\n[mcu]\n[ups]\n[telemetry]\n";
    assert_eq!(Config::missing_sections(old), vec!["oled", "lid"]);
    let current = "[fan]\n[mcu]\n[ups]\n[oled]\n[telemetry]\n[lid]\n";
    assert!(Config::missing_sections(current).is_empty());
    // An indented header still counts; a key whose value looks like one does not.
    assert!(
        Config::missing_sections("  [fan]\n[mcu]\n[ups]\n[oled]\n[telemetry]\n[lid]\n").is_empty()
    );
    assert!(Config::missing_sections("x = \"[oled]\"\n").contains(&"oled"));
    // The packaged config must have every section this version knows.
    let packaged = include_str!("../../../packaging/config/config.toml");
    assert!(
        Config::missing_sections(packaged).is_empty(),
        "packaged config is missing sections"
    );
}
