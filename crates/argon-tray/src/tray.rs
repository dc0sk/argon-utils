// SPDX-License-Identifier: GPL-3.0-or-later
//! The `StatusNotifierItem`: a thin shell around a rendered [`View`].

use crate::view::{Urgency, View};
use std::process::Command;

/// The tray item.
pub struct ArgonTray {
    /// What to show. Replaced wholesale on each change.
    pub view: View,
    /// The outcome of the last thing the user asked for, shown until the next.
    pub last_action: Option<String>,
}

impl ArgonTray {
    /// Creates the tray showing `view`.
    pub const fn new(view: View) -> Self {
        Self {
            view,
            last_action: None,
        }
    }
}

impl ksni::Tray for ArgonTray {
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

/// Cancels a scheduled poweroff as the logged-in user.
///
/// `shutdown -c` rather than a call into argond: an active local session may cancel a
/// scheduled shutdown under the default polkit policy, and argond already treats an
/// operator's cancellation as final for the current outage. Going through the daemon would
/// need an IPC channel that does not exist yet, for no gain in what can be done.
fn cancel_poweroff() -> String {
    match Command::new("shutdown").arg("-c").output() {
        Ok(o) if o.status.success() => {
            "Poweroff cancelled. argond will not reschedule it during this outage.".to_owned()
        }
        Ok(o) => format!(
            "Cancel failed: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        ),
        Err(e) => format!("Cancel failed: {e}"),
    }
}
