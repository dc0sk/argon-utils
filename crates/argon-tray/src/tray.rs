// SPDX-License-Identifier: GPL-3.0-or-later
//! The `StatusNotifierItem`: a thin shell around a rendered [`View`].

use crate::view::{Urgency, View};
use crate::wake::{self, Pending, Preset};
use std::process::Command;

/// The tray item.
pub struct ArgonTray {
    /// What to show. Replaced wholesale on each change.
    pub view: View,
    /// The outcome of the last thing the user asked for, shown until the next.
    pub last_action: Option<String>,
    /// What polkit says about powering off with a wake, via argond; `None` when argond is not on
    /// the bus. Refreshed in the background, so opening the menu never waits on it.
    pub can_wake: Option<String>,
    /// The wake times offered, refreshed with `can_wake`.
    pub presets: Vec<Preset>,
    /// Whatever shutdown logind has scheduled, whoever asked for it.
    pub pending: Option<Pending>,
    /// `(wake, poweroff)` of this tray's last successful "power off and wake", so the menu can
    /// say when the machine comes back.
    /// Shared with the poll loop, which matches it against what logind reports.
    pub ours: std::sync::Arc<std::sync::Mutex<Option<(u64, u64)>>>,
}

impl ArgonTray {
    /// Creates the tray showing `view`.
    pub fn new(view: View) -> Self {
        Self {
            view,
            last_action: None,
            can_wake: None,
            presets: Vec::new(),
            pending: None,
            ours: std::sync::Arc::default(),
        }
    }

    /// Asks argond to power off and wake at `at_unix`, and shows the result.
    fn power_off_and_wake(&mut self, at_unix: u64) {
        let (msg, ours) = wake_result(at_unix);
        self.last_action = Some(msg);
        if let Some((wake_unix, poweroff_unix)) = ours {
            if let Ok(mut o) = self.ours.lock() {
                *o = Some((wake_unix, poweroff_unix));
            }
            // Shown at once; the next logind read confirms it.
            self.pending = Some(Pending {
                kind: "poweroff".into(),
                at_unix: poweroff_unix,
                wake_unix: Some(wake_unix),
            });
        }
    }

    /// Cancels a shutdown that is not argond's battery poweroff, and shows the result.
    fn cancel_other(&mut self, woken: bool) {
        let (msg, cancelled) = cancel_other(woken);
        self.last_action = Some(msg);
        if cancelled {
            self.pending = None;
        }
    }
}

impl ksni::Tray for ArgonTray {
    /// No panel is hosting tray icons -- at login, before the panel has started, or because it
    /// was stopped. Keep waiting: ksni registers the icon as soon as a panel appears.
    fn watcher_offline(&self, reason: ksni::OfflineReason) -> bool {
        eprintln!("argon-tray: no panel is hosting tray icons ({reason:?}); waiting for one");
        true
    }

    fn watcher_online(&self) {
        eprintln!("argon-tray: a panel is hosting tray icons again; showing the icon");
    }

    fn id(&self) -> String {
        "argon-utils".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::Hardware
    }

    fn title(&self) -> String {
        "argon-utils".into()
    }

    fn status(&self) -> ksni::Status {
        // Never Passive: hosts may hide a passive item, and a UPS indicator that disappears
        // while everything is fine cannot be glanced at to confirm that it is.
        match self.view.urgency {
            Urgency::Normal => ksni::Status::Active,
            Urgency::Attention => ksni::Status::NeedsAttention,
        }
    }

    fn icon_name(&self) -> String {
        self.view.icon.into()
    }

    fn attention_icon_name(&self) -> String {
        self.view.icon.into()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            icon_name: self.view.icon.into(),
            icon_pixmap: Vec::new(),
            title: self.view.headline.clone(),
            description: self.view.details.join("\n"),
        }
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::{MenuItem, StandardItem, SubMenu};

        let label = |text: &str| -> MenuItem<Self> {
            StandardItem {
                label: text.to_owned(),
                enabled: false,
                ..Default::default()
            }
            .into()
        };

        let mut items = vec![label(&self.view.headline)];
        items.extend(self.view.details.iter().map(|d| label(d)));

        if self.view.shutdown_pending {
            items.push(MenuItem::Separator);
            // Two steps on purpose. Cancelling tells argond the operator has taken over for
            // this outage: it stands down, and if mains does not come back the UPS cuts power
            // when the battery is empty, with no clean shutdown. That is a legitimate choice,
            // but not one a stray click should make.
            items.push(
                SubMenu {
                    label: "Cancel the scheduled poweroff".into(),
                    icon_name: "process-stop-symbolic".into(),
                    submenu: vec![
                        StandardItem {
                            label: "Yes, keep running on battery".into(),
                            activate: Box::new(|this: &mut Self| {
                                this.last_action = Some(cancel_poweroff());
                            }),
                            ..Default::default()
                        }
                        .into(),
                        label("If mains stays off, the UPS will cut power when empty."),
                    ],
                    ..Default::default()
                }
                .into(),
            );
        }

        // A shutdown that is not argond's battery poweroff -- ours with a wake, or one the user
        // scheduled some other way. Cancelling it is the safe direction, so one step is enough.
        if let (false, Some(p)) = (self.view.shutdown_pending, &self.pending) {
            items.push(MenuItem::Separator);
            items.extend(pending_lines(p).iter().map(|l| label(l)));
            let woken = p.wake_unix.is_some();
            items.push(
                StandardItem {
                    label: format!("Cancel the {}", noun(&p.kind)),
                    icon_name: "process-stop-symbolic".into(),
                    activate: Box::new(move |this: &mut Self| this.cancel_other(woken)),
                    ..Default::default()
                }
                .into(),
            );
        }

        if wake::offer(
            self.can_wake.as_deref(),
            self.view.shutdown_pending || self.pending.is_some(),
        ) && !self.presets.is_empty()
        {
            items.push(MenuItem::Separator);
            // The submenu is the confirmation: every item in it states the consequence --
            // power off now -- in its label, and argond still announces the poweroff a minute
            // ahead with `shutdown -c` to cancel.
            items.push(
                SubMenu {
                    label: "Power off now…".into(),
                    icon_name: "system-shutdown-symbolic".into(),
                    submenu: self
                        .presets
                        .iter()
                        .map(|p| {
                            let at = p.at_unix;
                            StandardItem {
                                label: p.label.clone(),
                                activate: Box::new(move |this: &mut Self| {
                                    this.power_off_and_wake(at);
                                }),
                                ..Default::default()
                            }
                            .into()
                        })
                        .collect(),
                    ..Default::default()
                }
                .into(),
            );
        }

        if let Some(msg) = &self.last_action {
            items.push(MenuItem::Separator);
            items.push(label(msg));
        }

        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Quit the tray icon".into(),
                icon_name: "application-exit-symbolic".into(),
                activate: Box::new(|_| std::process::exit(0)),
                ..Default::default()
            }
            .into(),
        );
        items
    }
}

fn hhmm(unix: u64) -> String {
    argon_device::status::local_hhmm(std::time::UNIX_EPOCH + std::time::Duration::from_secs(unix))
}

/// What logind calls a shutdown kind, in words; `dry-` forms read as the real thing.
fn noun(kind: &str) -> &'static str {
    match kind.trim_start_matches("dry-") {
        "reboot" => "reboot",
        "halt" => "halt",
        _ => "poweroff",
    }
}

/// The menu lines describing a pending shutdown.
fn pending_lines(p: &Pending) -> Vec<String> {
    let mut lines = vec![format!(
        "A {} is scheduled for {}.",
        noun(&p.kind),
        hhmm(p.at_unix)
    )];
    if let Some(w) = p.wake_unix {
        lines.push(format!("The UPS will wake it at {}.", hhmm(w)));
    }
    lines
}

/// Asks argond to power off and wake. Says what happened, and on success returns
/// `(wake, poweroff)`.
fn wake_result(at_unix: u64) -> (String, Option<(u64, u64)>) {
    match wake::poweroff_with_wake(at_unix) {
        Ok((wake_unix, poweroff_unix)) => (
            format!(
                "Powering off at {}. The UPS will wake it at {}.",
                hhmm(poweroff_unix),
                hhmm(wake_unix)
            ),
            Some((wake_unix, poweroff_unix)),
        ),
        Err(e) => (format!("Not powered off: {e}"), None),
    }
}

/// Cancels a shutdown that is not argond's battery poweroff. Says what happened, and whether it
/// was cancelled.
fn cancel_other(woken: bool) -> (String, bool) {
    match shutdown_c() {
        // The wake stays set in the UPS until then: argond's safety net moves it out of the
        // way before it can come due on the running machine.
        Ok(()) if woken => (
            "Cancelled. argond will clear the UPS wake before it comes due.".to_owned(),
            true,
        ),
        Ok(()) => ("Cancelled.".to_owned(), true),
        Err(e) => (format!("Cancel failed: {e}"), false),
    }
}

fn shutdown_c() -> Result<(), String> {
    match Command::new("shutdown").arg("-c").output() {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err(String::from_utf8_lossy(&o.stderr).trim().to_owned()),
        Err(e) => Err(e.to_string()),
    }
}

/// Cancels a scheduled poweroff as the logged-in user.
///
/// `shutdown -c` rather than a call into argond: an active local session may cancel a
/// scheduled shutdown under the default polkit policy, and argond already treats an
/// operator's cancellation as final for the current outage. Going through the daemon would
/// need an IPC channel that does not exist yet, for no gain in what can be done.
fn cancel_poweroff() -> String {
    match shutdown_c() {
        Ok(()) => {
            "Poweroff cancelled. argond will not reschedule it during this outage.".to_owned()
        }
        Err(e) => format!("Cancel failed: {e}"),
    }
}
