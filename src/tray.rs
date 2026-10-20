//! Tray icon (StatusNotifierItem): the Linux counterpart of the menu bar paw.
//! GNOME shows it through the AppIndicator extension; KDE and most other
//! desktops show it natively.

use ksni::menu::{CheckmarkItem, StandardItem, SubMenu};
use ksni::{MenuItem, ToolTip};

use crate::config::APP_ID;

#[derive(Debug, Clone, Copy)]
pub enum TrayAction {
    Toggle,
    LockWith(usize),
    SetSmart(bool),
    OpenSettings,
    Quit,
}

pub struct Tray {
    pub locked: bool,
    pub smart: bool,
    pub profiles: Vec<String>,
    pub icon_theme_path: String,
    pub tx: async_channel::Sender<TrayAction>,
}

impl Tray {
    fn send(&self, action: TrayAction) {
        let _ = self.tx.try_send(action);
    }
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "catlock".into()
    }

    fn title(&self) -> String {
        "Catlock".into()
    }

    fn icon_name(&self) -> String {
        if self.locked { format!("{APP_ID}-locked-symbolic") } else { format!("{APP_ID}-symbolic") }
    }

    fn icon_theme_path(&self) -> String {
        self.icon_theme_path.clone()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "Catlock".into(),
            description: if self.locked { "Keyboard locked" } else { "Keyboard unlocked" }.into(),
            ..Default::default()
        }
    }

    /// A plain click locks or unlocks; the menu holds everything else.
    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(TrayAction::Toggle);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let profiles = self
            .profiles
            .iter()
            .enumerate()
            .map(|(i, name)| {
                StandardItem {
                    label: name.clone(),
                    enabled: !self.locked,
                    activate: Box::new(move |t: &mut Self| t.send(TrayAction::LockWith(i))),
                    ..Default::default()
                }
                .into()
            })
            .collect();
        vec![
            StandardItem {
                label: if self.locked { "Unlock Keyboard" } else { "Lock Keyboard" }.into(),
                icon_name: if self.locked { "changes-allow-symbolic" } else { "changes-prevent-symbolic" }.into(),
                activate: Box::new(|t: &mut Self| t.send(TrayAction::Toggle)),
                ..Default::default()
            }
            .into(),
            SubMenu { label: "Lock With Profile".into(), submenu: profiles, ..Default::default() }.into(),
            MenuItem::Separator,
            CheckmarkItem {
                label: "Smart Detect".into(),
                checked: self.smart,
                activate: Box::new(|t: &mut Self| t.send(TrayAction::SetSmart(!t.smart))),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Settings…".into(),
                activate: Box::new(|t: &mut Self| t.send(TrayAction::OpenSettings)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit-symbolic".into(),
                activate: Box::new(|t: &mut Self| t.send(TrayAction::Quit)),
                ..Default::default()
            }
            .into(),
        ]
    }
}
