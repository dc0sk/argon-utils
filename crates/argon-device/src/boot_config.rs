// SPDX-License-Identifier: GPL-3.0-or-later
//! Reading `config.txt`: which directives are actually in force on this board.
//!
//! Pure parsing, no I/O, so the rules can be tested without a Raspberry Pi.
//!
//! The file is not a flat list of settings. It is divided by **conditional filters** --
//! `[pi4]`, `[pi5]`, `[cm5]`, `[all]`, `[none]` and others -- and a directive only applies when
//! its filter matches the board the file is booting. A check that greps for a line gets this
//! wrong in both directions: it reports a `[pi5]`-only overlay as present on a Pi 4, and a
//! commented-out line as configured.

/// Where a directive was found, and whether it counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Directive {
    /// The filter section it sits under; `None` before any filter, which applies to every board.
    pub section: Option<String>,
    /// Everything after the `=`, trimmed.
    pub value: String,
    /// Whether the line is commented out.
    pub commented: bool,
    /// 1-based line number, for telling a person where to look.
    pub line: usize,
}

impl Directive {
    /// Whether this directive is in force for a board matching `sections`.
    ///
    /// `sections` are the filter names that match the board, lowercase and without brackets:
    /// `["all", "pi4"]` for a Pi 4, plus `"cm5"` and so on where they apply. A directive before
    /// any filter applies always; `[none]` matches nothing and so is never included.
    #[must_use]
    pub fn applies_to(&self, sections: &[&str]) -> bool {
        if self.commented {
            return false;
        }
        self.section
            .as_ref()
            .is_none_or(|s| sections.iter().any(|w| w.eq_ignore_ascii_case(s)))
    }
}

/// Finds every occurrence of `key`, in order, commented ones included.
///
/// `key` is matched case-insensitively on the part before `=`, so `dtoverlay` finds
/// `dtoverlay=gpio-ir,gpio_pin=23` and reports the whole right-hand side as the value.
#[must_use]
pub fn find(text: &str, key: &str) -> Vec<Directive> {
    let mut section: Option<String> = None;
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = Some(name.trim().to_owned());
            continue;
        }
        let (commented, body) = match line.strip_prefix('#') {
            Some(rest) => (true, rest.trim()),
            None => (false, line),
        };
        let Some((k, v)) = body.split_once('=') else {
            continue;
        };
        if k.trim().eq_ignore_ascii_case(key) {
            out.push(Directive {
                section: section.clone(),
                value: v.trim().to_owned(),
                commented,
                line: i + 1,
            });
        }
    }
    out
}

/// Whether a directive with this exact value is in force for the given board.
///
/// Values are compared with whitespace trimmed and case ignored, so
/// `dtparam=i2c_arm=on` matches however it is spaced.
#[must_use]
pub fn is_active(text: &str, key: &str, value: &str, sections: &[&str]) -> bool {
    find(text, key)
        .iter()
        .any(|d| d.applies_to(sections) && d.value.eq_ignore_ascii_case(value.trim()))
}

/// Whether any directive with this key *starts with* `prefix` and is in force.
///
/// For overlays, where the parameters vary: `dtoverlay=gpio-ir,gpio_pin=23` is still the
/// `gpio-ir` overlay.
#[must_use]
pub fn is_active_prefix(text: &str, key: &str, prefix: &str, sections: &[&str]) -> bool {
    find(text, key).iter().any(|d| {
        d.applies_to(sections)
            && d.value
                .to_ascii_lowercase()
                .starts_with(&prefix.to_ascii_lowercase())
    })
}

/// The filter sections that match a board, given its model string.
///
/// Conservative: a model we do not recognise gets `["all"]`, so a directive under a specific
/// filter is not claimed to be in force when we cannot tell.
#[must_use]
pub fn sections_for(model: &str) -> Vec<&'static str> {
    let m = model.to_ascii_lowercase();
    let mut out = vec!["all"];
    if m.contains("compute module 5") || m.contains("cm5") {
        out.extend(["cm5", "pi5"]);
    } else if m.contains("pi 5") {
        out.push("pi5");
    } else if m.contains("compute module 4") || m.contains("cm4") {
        out.extend(["cm4", "pi4"]);
    } else if m.contains("pi 4") || m.contains("pi 400") {
        out.push("pi4");
    } else if m.contains("pi 3") {
        out.push("pi3");
    } else if m.contains("pi zero 2") {
        out.extend(["pi02", "pi0"]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# a comment
#dtparam=i2c_arm=on
dtparam=audio=on

[cm5]
dtoverlay=dwc2,dr_mode=host

[pi5]
dtoverlay=nospi10

[all]
enable_uart=1
dtparam=i2c_arm=on
dtoverlay=gpio-ir,gpio_pin=23
";

    #[test]
    fn a_commented_line_is_not_configuration() {
        // The stock config.txt ships `#dtparam=i2c_arm=on`, and a grep would call that done.
        let found = find(SAMPLE, "dtparam");
        let commented: Vec<_> = found.iter().filter(|d| d.commented).collect();
        assert_eq!(commented.len(), 1);
        assert!(!commented[0].applies_to(&["all", "pi4"]));
        // The real one, later in [all], does apply.
        assert!(is_active(SAMPLE, "dtparam", "i2c_arm=on", &["all", "pi4"]));
    }

    #[test]
    fn a_filter_section_limits_where_a_directive_applies() {
        // dwc2 sits under [cm5]: in force on a CM5, absent on a Pi 4.
        assert!(is_active_prefix(
            SAMPLE,
            "dtoverlay",
            "dwc2",
            &["all", "cm5", "pi5"]
        ));
        assert!(!is_active_prefix(
            SAMPLE,
            "dtoverlay",
            "dwc2",
            &["all", "pi4"]
        ));
    }

    #[test]
    fn a_directive_before_any_filter_applies_to_every_board() {
        assert!(is_active(SAMPLE, "dtparam", "audio=on", &["all", "pi4"]));
        assert!(is_active(SAMPLE, "dtparam", "audio=on", &["all", "pi5"]));
    }

    #[test]
    fn an_overlay_is_recognised_whatever_its_parameters() {
        assert!(is_active_prefix(
            SAMPLE,
            "dtoverlay",
            "gpio-ir",
            &["all", "pi4"]
        ));
        assert!(!is_active_prefix(
            SAMPLE,
            "dtoverlay",
            "gpio-ir-tx",
            &["all", "pi4"]
        ));
    }

    #[test]
    fn sections_are_derived_from_the_model_and_fall_back_to_all() {
        assert_eq!(
            sections_for("Raspberry Pi 4 Model B Rev 1.4"),
            ["all", "pi4"]
        );
        assert_eq!(
            sections_for("Raspberry Pi 5 Model B Rev 1.1"),
            ["all", "pi5"]
        );
        assert_eq!(
            sections_for("Raspberry Pi Compute Module 5 Rev 1.0"),
            ["all", "cm5", "pi5"]
        );
        // Unknown board: only [all], so nothing under a specific filter is claimed active.
        assert_eq!(sections_for("Some Other Board"), ["all"]);
    }

    #[test]
    fn the_line_number_is_reported_so_a_person_can_go_and_look() {
        let d = find(SAMPLE, "enable_uart");
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].line, 12);
        assert_eq!(d[0].section.as_deref(), Some("all"));
    }
}
