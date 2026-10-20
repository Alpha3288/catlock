//! The GTK side: one controller owning config, lock state and every surface
//! (settings window, lock screen, tray, notifications).

pub mod heatmap;
pub mod overlay;
pub mod window;

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::{gio, glib};
use ksni::TrayMethods;
use tokio::sync::mpsc;

use crate::config::{APP_ID, Config, LockMode};
use crate::engine::{self, Command, DeviceInfo, EngineConfig, Event, LockReason, UnlockReason};
use crate::sound::Player;
use crate::system;
use crate::tray::{Tray, TrayAction};
use overlay::{Overlay, Purpose};

#[derive(Clone, Copy, Debug)]
pub struct LockState {
    pub reason: LockReason,
    pub mode: LockMode,
    pub touchpad: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Devices {
    pub keyboards: Vec<DeviceInfo>,
    pub touchpads: Vec<String>,
    pub denied: usize,
}

type OverlaySlot = Rc<RefCell<Option<Overlay>>>;

pub struct Controller {
    pub app: adw::Application,
    pub cfg: RefCell<Config>,
    pub player: Player,
    pub lock: Cell<Option<LockState>>,
    pub devices: RefCell<Devices>,
    pub failure: RefCell<Option<String>>,
    engine: mpsc::UnboundedSender<Command>,
    rt: tokio::runtime::Handle,
    tray: RefCell<Option<ksni::Handle<Tray>>>,
    overlay: RefCell<Option<OverlaySlot>>,
    window: RefCell<Option<window::Window>>,
    inhibit: Cell<Option<u32>>,
    subscriptions: RefCell<Vec<gio::SignalSubscription>>,
    hold: RefCell<Option<gio::ApplicationHoldGuard>>,
}

fn engine_config(cfg: &Config) -> EngineConfig {
    EngineConfig {
        shortcut: cfg.shortcut,
        smart: cfg.smart,
        mode: cfg.profile().mode,
        lock_touchpad: cfg.profile().lock_touchpad,
    }
}

/// Where the paw icons live when running from the source tree; installed
/// copies are found through the regular icon theme.
pub fn dev_icon_dir() -> Option<&'static str> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/data/icons");
    std::path::Path::new(dir).exists().then_some(dir)
}

impl Controller {
    pub fn new(app: &adw::Application, rt: tokio::runtime::Handle) -> Rc<Self> {
        let cfg = Config::load();
        let (events_tx, events_rx) = async_channel::unbounded();
        let engine = engine::spawn(&rt, engine_config(&cfg), events_tx);

        let ctl = Rc::new(Self {
            app: app.clone(),
            cfg: RefCell::new(cfg),
            player: Player::spawn(),
            lock: Cell::new(None),
            devices: RefCell::default(),
            failure: RefCell::new(None),
            engine,
            rt,
            tray: RefCell::new(None),
            overlay: RefCell::new(None),
            window: RefCell::new(None),
            inhibit: Cell::new(None),
            subscriptions: RefCell::new(Vec::new()),
            hold: RefCell::new(Some(app.hold())),
        });

        let weak = Rc::downgrade(&ctl);
        glib::spawn_future_local(async move {
            while let Ok(event) = events_rx.recv().await {
                let Some(ctl) = weak.upgrade() else { break };
                ctl.on_engine(event);
            }
        });

        ctl.spawn_tray();
        let weak = Rc::downgrade(&ctl);
        *ctl.subscriptions.borrow_mut() = system::watch_screen_away(move || {
            if let Some(ctl) = weak.upgrade() {
                ctl.unlock(UnlockReason::ScreenLocked);
            }
        });
        ctl.install_actions();
        ctl
    }

    fn spawn_tray(self: &Rc<Self>) {
        let (tx, rx) = async_channel::unbounded();
        let cfg = self.cfg.borrow();
        let tray = Tray {
            locked: false,
            smart: cfg.smart.enabled,
            profiles: cfg.profiles.iter().map(|p| p.name.clone()).collect(),
            icon_theme_path: dev_icon_dir().unwrap_or_default().into(),
            tx,
        };
        // Without a tray host yet (e.g. the GNOME AppIndicator extension still
        // loading), the icon registers as soon as one appears.
        let spawned = self.rt.spawn(async move { tray.assume_sni_available(true).spawn().await });
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            match spawned.await {
                Ok(Ok(handle)) => {
                    if let Some(ctl) = weak.upgrade() {
                        *ctl.tray.borrow_mut() = Some(handle);
                        ctl.update_tray();
                    }
                }
                Ok(Err(e)) => eprintln!("catlock: no tray icon ({e}); use the window or the CLI"),
                Err(e) => eprintln!("catlock: tray task failed: {e}"),
            }
        });
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            while let Ok(action) = rx.recv().await {
                let Some(ctl) = weak.upgrade() else { break };
                match action {
                    TrayAction::Toggle => ctl.toggle(),
                    TrayAction::LockWith(i) => ctl.lock_now(Some(i)),
                    TrayAction::SetSmart(on) => {
                        ctl.edit(|c| c.smart.enabled = on);
                        ctl.with_window(|w| w.reload());
                    }
                    TrayAction::OpenSettings => ctl.show_window(),
                    TrayAction::Quit => ctl.quit(),
                }
            }
        });
    }

    fn install_actions(self: &Rc<Self>) {
        let action = |name: &str, f: fn(&Rc<Controller>)| {
            let weak: Weak<Controller> = Rc::downgrade(self);
            gio::ActionEntry::builder(name)
                .activate(move |_: &adw::Application, _, _| {
                    if let Some(ctl) = weak.upgrade() {
                        f(&ctl);
                    }
                })
                .build()
        };
        self.app.add_action_entries([
            action("lock", |c| c.lock_now(None)),
            action("unlock", |c| c.unlock(UnlockReason::Manual)),
            action("toggle", |c| c.toggle()),
            action("settings", |c| c.show_window()),
            action("free-touchpad", |c| c.send(Command::SetTouchpadLocked(false))),
            action("not-my-cat", |c| c.not_my_cat()),
            action("quit", |c| c.quit()),
        ]);
    }

    pub fn send(&self, cmd: Command) {
        let _ = self.engine.send(cmd);
    }

    /// Saves the config and pushes it to the engine and tray.
    pub fn apply(&self) {
        let cfg = self.cfg.borrow();
        cfg.save();
        self.send(Command::Configure(engine_config(&cfg)));
        drop(cfg);
        self.update_tray();
    }

    pub fn edit(&self, f: impl FnOnce(&mut Config)) {
        f(&mut self.cfg.borrow_mut());
        self.apply();
    }

    fn update_tray(&self) {
        let Some(handle) = self.tray.borrow().clone() else { return };
        let cfg = self.cfg.borrow();
        let locked = self.lock.get().is_some();
        let smart = cfg.smart.enabled;
        let profiles: Vec<String> = cfg.profiles.iter().map(|p| p.name.clone()).collect();
        self.rt.spawn(async move {
            handle
                .update(move |t: &mut Tray| {
                    t.locked = locked;
                    t.smart = smart;
                    t.profiles = profiles;
                })
                .await;
        });
    }

    pub fn with_window(&self, f: impl FnOnce(&window::Window)) {
        if let Some(window) = self.window.borrow().as_ref() {
            f(window);
        }
    }

    pub fn show_window(self: &Rc<Self>) {
        if self.window.borrow().is_none() {
            let window = window::Window::new(self);
            *self.window.borrow_mut() = Some(window);
        }
        self.with_window(|w| w.present());
        #[cfg(debug_assertions)]
        if let Some(dir) = std::env::var_os("CATLOCK_SNAPSHOT") {
            self.with_window(|w| w.snapshot_pages(dir.into()));
        }
    }

    pub fn lock_now(self: &Rc<Self>, profile: Option<usize>) {
        if self.lock.get().is_some() {
            return;
        }
        if let Some(i) = profile.filter(|&i| i < self.cfg.borrow().profiles.len()) {
            self.edit(|c| c.active_profile = i);
            self.with_window(|w| w.reload());
        }
        let (mode, lock_touchpad) = {
            let cfg = self.cfg.borrow();
            (cfg.profile().mode, cfg.profile().lock_touchpad)
        };
        self.send(Command::Lock { mode, lock_touchpad });
    }

    pub fn unlock(&self, reason: UnlockReason) {
        self.send(Command::Unlock(reason));
    }

    pub fn toggle(self: &Rc<Self>) {
        if self.lock.get().is_some() {
            self.unlock(UnlockReason::Manual);
        } else {
            self.lock_now(None);
        }
    }

    fn not_my_cat(self: &Rc<Self>) {
        if let Some(LockState { reason: LockReason::Smart(rule), .. }) = self.lock.get() {
            self.edit(|c| c.smart.learning.forgive(rule));
            self.with_window(|w| w.reload());
        }
        self.unlock(UnlockReason::NotMyCat);
    }

    pub fn set_monitor(&self, on: bool) {
        self.send(Command::Monitor(on));
    }

    pub fn preview_lock_screen(&self) {
        let cfg = self.cfg.borrow();
        let slot = Overlay::show(&self.app, &cfg.profile().screen, &cfg.shortcut, Purpose::Preview);
        drop(slot);
    }

    fn quit(&self) {
        self.hold.borrow_mut().take();
        self.app.quit();
    }

    fn notify_locked(&self, state: LockState) {
        let shortcut = self.cfg.borrow().shortcut;
        let title = match state.mode {
            LockMode::BuiltIn => "Built-in keyboard locked",
            _ => "Keyboard locked",
        };
        let mut body = format!("Press {shortcut} to unlock.");
        if state.touchpad {
            body.push_str(" The touchpad is locked too.");
        }
        let n = gio::Notification::new(title);
        n.set_body(Some(&body));
        n.set_icon(&gio::ThemedIcon::new(&format!("{APP_ID}-locked-symbolic")));
        n.add_button("Unlock", "app.unlock");
        if state.touchpad {
            n.add_button("Free Touchpad", "app.free-touchpad");
        }
        if matches!(state.reason, LockReason::Smart(_)) {
            n.add_button("That Wasn't My Cat", "app.not-my-cat");
        }
        n.set_priority(gio::NotificationPriority::High);
        self.app.send_notification(Some("locked"), &n);
    }

    fn notify(&self, id: &str, title: &str, body: &str) {
        let n = gio::Notification::new(title);
        n.set_body(Some(body));
        self.app.send_notification(Some(id), &n);
    }

    fn on_engine(self: &Rc<Self>, event: Event) {
        match event {
            Event::Devices { keyboards, touchpads, denied } => {
                *self.devices.borrow_mut() = Devices { keyboards, touchpads, denied };
                self.with_window(|w| w.refresh_status());
            }
            Event::Locked { reason, mode, touchpad } => {
                let state = LockState { reason, mode, touchpad };
                self.lock.set(Some(state));
                let profile = self.cfg.borrow().profile().clone();
                if profile.keep_awake {
                    let flags = gtk::ApplicationInhibitFlags::IDLE | gtk::ApplicationInhibitFlags::SUSPEND;
                    let cookie = self.app.inhibit(None::<&gtk::Window>, flags, Some("Keyboard locked by Catlock"));
                    self.inhibit.set(Some(cookie));
                }
                if mode == LockMode::Full {
                    let smart = match reason {
                        LockReason::Smart(rule) => Some(rule),
                        _ => None,
                    };
                    let weak = Rc::downgrade(self);
                    let on_not_my_cat: Rc<dyn Fn()> = Rc::new(move || {
                        if let Some(ctl) = weak.upgrade() {
                            ctl.not_my_cat();
                        }
                    });
                    let shortcut = self.cfg.borrow().shortcut;
                    let slot =
                        Overlay::show(&self.app, &profile.screen, &shortcut, Purpose::Lock { smart, on_not_my_cat });
                    *self.overlay.borrow_mut() = Some(slot);
                } else {
                    self.notify_locked(state);
                }
                self.update_tray();
                self.with_window(|w| w.refresh_status());
            }
            Event::Unlocked(reason) => {
                self.lock.set(None);
                if let Some(slot) = self.overlay.borrow_mut().take() {
                    overlay::close(&slot);
                }
                self.app.withdraw_notification("locked");
                if let Some(cookie) = self.inhibit.take() {
                    self.app.uninhibit(cookie);
                }
                if reason == UnlockReason::ExternalKeyboardGone {
                    self.notify("unlocked", "Keyboard unlocked", "The external keyboard was unplugged.");
                }
                self.update_tray();
                self.with_window(|w| w.refresh_status());
            }
            Event::TouchpadLocked(on) => {
                if let Some(mut state) = self.lock.get() {
                    state.touchpad = on;
                    self.lock.set(Some(state));
                    if state.mode != LockMode::Full {
                        self.notify_locked(state);
                    }
                }
            }
            Event::Blocked => {
                let Some(state) = self.lock.get() else { return };
                if state.mode != LockMode::Full {
                    return;
                }
                let profile = self.cfg.borrow().profile().clone();
                self.player.deter(&profile.sound, profile.volume);
                if profile.screen.flash
                    && let Some(slot) = self.overlay.borrow().as_ref()
                    && let Some(overlay) = slot.borrow().as_ref()
                {
                    overlay.flash();
                }
            }
            Event::EscapeWhileLocked => {
                if let Some(state) = self.lock.get().filter(|s| s.mode != LockMode::Full) {
                    self.notify_locked(state);
                }
            }
            Event::Key { code, down } => self.with_window(|w| w.key(code, down)),
            Event::Chaos(level) => self.with_window(|w| w.chaos(level)),
            Event::ShortcutRecorded(shortcut) => {
                self.edit(|c| c.shortcut = shortcut);
                self.with_window(|w| w.shortcut_recorded());
            }
            Event::Failed(message) => {
                eprintln!("catlock: {message}");
                self.notify("failed", "Catlock cannot lock the keyboard", &message);
                *self.failure.borrow_mut() = Some(message);
                self.with_window(|w| w.refresh_status());
            }
        }
    }

    /// Handles a command line from this or a second `catlock` invocation.
    pub fn command_line(self: &Rc<Self>, cmd: &gio::ApplicationCommandLine, first: bool) -> i32 {
        let args: Vec<String> = cmd.arguments().iter().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
        let mut show = !first || args.is_empty();
        let mut iter = args.iter().peekable();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--background" => show = false,
                "--settings" => show = true,
                "--lock" => {
                    show = false;
                    let profile = iter.next_if(|a| !a.starts_with("--")).and_then(|name| {
                        let cfg = self.cfg.borrow();
                        name.parse::<usize>()
                            .ok()
                            .map(|n| n.saturating_sub(1))
                            .or_else(|| cfg.profiles.iter().position(|p| p.name.eq_ignore_ascii_case(name)))
                    });
                    self.lock_now(profile);
                }
                "--unlock" => {
                    show = false;
                    self.unlock(UnlockReason::Manual);
                }
                "--toggle" => {
                    show = false;
                    self.toggle();
                }
                "--status" => {
                    show = false;
                    let status = match self.lock.get() {
                        Some(s) => format!("locked ({})\n", s.mode.label().to_lowercase()),
                        None => "unlocked\n".into(),
                    };
                    cmd.print_literal(&status);
                }
                "--quit" => {
                    self.quit();
                    return 0;
                }
                "-h" | "--help" => {
                    cmd.print_literal(HELP);
                    return 0;
                }
                other => {
                    cmd.printerr_literal(&format!("catlock: unknown option {other}\n\n{HELP}"));
                    return 2;
                }
            }
        }
        if show {
            self.show_window();
        }
        0
    }
}

pub const HELP: &str = "Usage: catlock [OPTION]

Keyboard lock for cat people. The first run starts Catlock and keeps it
running in the tray; later runs control the running instance.

  --background        start without opening the settings window
  --settings          open the settings window
  --lock [PROFILE]    lock now, optionally with a profile (name or number)
  --unlock            unlock
  --toggle            lock or unlock
  --status            print whether the keyboard is locked
  --quit              stop Catlock (the keyboards are released)
  -h, --help          show this help
";
