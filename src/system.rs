//! Desktop integration: screen-lock and sleep signals, launch at login.

use std::path::PathBuf;

use gtk::gio;
use gtk::glib;

use crate::config::APP_ID;

/// Calls `on_away` whenever the session locks, the screensaver starts or the
/// machine is about to sleep. The keyboard must never stay locked then, or the
/// password prompt would be unreachable. The returned subscriptions must be
/// kept alive for the callbacks to keep firing.
pub fn watch_screen_away(on_away: impl Fn() + Clone + 'static) -> Vec<gio::SignalSubscription> {
    let mut subscriptions = Vec::new();
    let active_true = |params: &glib::Variant| params.get::<(bool,)>().is_some_and(|(active,)| active);

    if let Ok(session) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) {
        for iface in ["org.gnome.ScreenSaver", "org.freedesktop.ScreenSaver"] {
            let on_away = on_away.clone();
            subscriptions.push(session.subscribe_to_signal(
                None,
                Some(iface),
                Some("ActiveChanged"),
                None,
                None,
                gio::DBusSignalFlags::NONE,
                move |signal| {
                    if active_true(signal.parameters) {
                        on_away();
                    }
                },
            ));
        }
    }
    if let Ok(system) = gio::bus_get_sync(gio::BusType::System, gio::Cancellable::NONE) {
        let sleep = on_away.clone();
        subscriptions.push(system.subscribe_to_signal(
            Some("org.freedesktop.login1"),
            Some("org.freedesktop.login1.Manager"),
            Some("PrepareForSleep"),
            Some("/org/freedesktop/login1"),
            None,
            gio::DBusSignalFlags::NONE,
            move |signal| {
                if active_true(signal.parameters) {
                    sleep();
                }
            },
        ));
        // Session.Lock is emitted on every session; acting on another user's
        // only unlocks our keyboard early, which is the safe direction.
        subscriptions.push(system.subscribe_to_signal(
            Some("org.freedesktop.login1"),
            Some("org.freedesktop.login1.Session"),
            Some("Lock"),
            None,
            None,
            gio::DBusSignalFlags::NONE,
            move |_| on_away(),
        ));
    }
    subscriptions
}

fn autostart_file() -> PathBuf {
    glib::user_config_dir().join("autostart").join(format!("{APP_ID}.desktop"))
}

pub fn launch_at_login() -> bool {
    autostart_file().exists()
}

pub fn set_launch_at_login(on: bool) -> std::io::Result<()> {
    let path = autostart_file();
    if !on {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    let exe = std::env::current_exe()?;
    std::fs::create_dir_all(path.parent().expect("autostart dir"))?;
    std::fs::write(
        path,
        format!(
            "[Desktop Entry]\nType=Application\nName=Catlock\nComment=Keyboard lock for cat people\n\
             Exec={} --background\nIcon={APP_ID}\nX-GNOME-Autostart-enabled=true\nNoDisplay=true\n",
            exe.display()
        ),
    )
}
