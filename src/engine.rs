//! Input engine: owns the keyboards, the virtual keyboard and the lock state.
//!
//! Every physical keyboard is grabbed for as long as Catlock runs and its
//! events are re-emitted through one virtual keyboard. Locking therefore just
//! stops forwarding, which works the same on X11 and every Wayland compositor
//! and lets the engine release held keys cleanly. If the process dies, the
//! kernel drops the grabs and the keyboards work directly again.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Context;
use evdev::{BusType, Device, EventStream, EventType, InputEvent, KeyCode as K};
use inotify::{Inotify, WatchMask};
use tokio::sync::mpsc;
use tokio_stream::{StreamExt, StreamMap};

use crate::config::{LockMode, Shortcut};
use crate::detect::{self, Detector, Rule};
use crate::keys;
use crate::uinput::{self, VirtualKeyboard};

const BUS_I2C: u16 = 0x18;

#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub shortcut: Shortcut,
    pub smart: detect::Settings,
    pub mode: LockMode,
    pub lock_touchpad: bool,
}

#[derive(Debug)]
pub enum Command {
    Configure(EngineConfig),
    Lock {
        mode: LockMode,
        lock_touchpad: bool,
    },
    Unlock(UnlockReason),
    SetTouchpadLocked(bool),
    /// Stream key activity and chaos levels for the Smart Detect page.
    Monitor(bool),
    RecordShortcut(bool),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockReason {
    Manual,
    Shortcut,
    Smart(Rule),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnlockReason {
    Manual,
    Shortcut,
    NotMyCat,
    ExternalKeyboardGone,
    ScreenLocked,
}

#[derive(Clone, Debug)]
pub struct DeviceInfo {
    pub name: String,
    pub built_in: bool,
}

#[derive(Clone, Debug)]
pub enum Event {
    Devices {
        keyboards: Vec<DeviceInfo>,
        touchpads: Vec<String>,
        denied: usize,
    },
    Locked {
        reason: LockReason,
        mode: LockMode,
        touchpad: bool,
    },
    Unlocked(UnlockReason),
    TouchpadLocked(bool),
    /// A key was swallowed by the lock.
    Blocked,
    EscapeWhileLocked,
    Key {
        code: u16,
        down: bool,
    },
    /// Every physical key currently down, sent when the live view starts.
    KeysHeld(Vec<u16>),
    Chaos(f32),
    ShortcutRecorded(Shortcut),
    Failed(String),
}

pub fn spawn(
    rt: &tokio::runtime::Handle,
    cfg: EngineConfig,
    events: async_channel::Sender<Event>,
) -> mpsc::UnboundedSender<Command> {
    let (tx, rx) = mpsc::unbounded_channel();
    rt.spawn(async move {
        if let Err(e) = run(cfg, rx, events.clone()).await {
            let _ = events.send(Event::Failed(format!("{e:#}"))).await;
        }
    });
    tx
}

enum Kind {
    Keyboard {
        built_in: bool,
    },
    /// The laptop's own touchpad or pointing stick.
    BuiltInPointer,
    Other,
}

fn classify(dev: &Device) -> Kind {
    let bus = dev.input_id().bus_type();
    if bus == BusType::BUS_VIRTUAL || dev.name() == Some(uinput::NAME) {
        return Kind::Other;
    }
    let Some(keys) = dev.supported_keys() else { return Kind::Other };
    if keys.contains(K::KEY_A) && keys.contains(K::KEY_SPACE) {
        return Kind::Keyboard { built_in: bus == BusType::BUS_I8042 };
    }
    let pointer = keys.contains(K::BTN_TOUCH) || keys.contains(K::BTN_LEFT);
    if pointer && (bus == BusType::BUS_I8042 || bus.0 == BUS_I2C) {
        return Kind::BuiltInPointer;
    }
    Kind::Other
}

struct Keyboard {
    name: String,
    built_in: bool,
    /// Has typed since it was connected. Security keys such as a YubiKey
    /// present themselves as keyboards but cannot type an unlock shortcut,
    /// so only keyboards seen typing count as a way out of a built-in lock.
    typed: bool,
}

struct Engine {
    cfg: EngineConfig,
    virt: VirtualKeyboard,
    streams: StreamMap<PathBuf, EventStream>,
    keyboards: HashMap<PathBuf, Keyboard>,
    pointers: HashMap<PathBuf, Device>,
    pointers_locked: bool,
    denied: HashSet<PathBuf>,
    /// Keys down on any physical keyboard, whether forwarded or not.
    physical: HashSet<u16>,
    /// Keys down on the virtual keyboard, as the compositor sees them.
    forwarded: HashSet<u16>,
    pending: Vec<(u16, u16, i32)>,
    leds: HashMap<u16, i32>,
    lock: Option<LockMode>,
    detector: Detector,
    monitor: bool,
    recording: bool,
    started: Instant,
    events: async_channel::Sender<Event>,
}

async fn run(
    cfg: EngineConfig,
    mut commands: mpsc::UnboundedReceiver<Command>,
    events: async_channel::Sender<Event>,
) -> anyhow::Result<()> {
    let virt =
        VirtualKeyboard::create().context("cannot create the virtual keyboard: is your user in the 'input' group?")?;
    let inotify = Inotify::init().context("cannot watch /dev/input")?;
    inotify.watches().add("/dev/input", WatchMask::CREATE | WatchMask::ATTRIB | WatchMask::DELETE)?;
    let mut hotplug = inotify.into_event_stream([0u8; 4096])?;

    let mut engine = Engine {
        detector: Detector::new(cfg.smart),
        cfg,
        virt,
        streams: StreamMap::new(),
        keyboards: HashMap::new(),
        pointers: HashMap::new(),
        pointers_locked: false,
        denied: HashSet::new(),
        physical: HashSet::new(),
        forwarded: HashSet::new(),
        pending: Vec::new(),
        leds: HashMap::new(),
        lock: None,
        monitor: false,
        recording: false,
        started: Instant::now(),
        events,
    };

    let mut nodes: Vec<PathBuf> = std::fs::read_dir("/dev/input")?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| is_event_node(p))
        .collect();
    nodes.sort();
    for path in nodes {
        engine.add(path).await;
    }
    engine.publish_devices();

    let mut tick = tokio::time::interval(Duration::from_millis(50));
    loop {
        tokio::select! {
            Some((path, result)) = engine.streams.next() => match result {
                Ok(ev) => engine.handle(&path, ev),
                Err(_) => engine.remove(&path),
            },
            leds = engine.virt.read_leds() => engine.mirror_leds(leds?),
            cmd = commands.recv() => match cmd {
                Some(cmd) => engine.command(cmd),
                None => return Ok(()),
            },
            Some(Ok(change)) = hotplug.next() => {
                let Some(name) = change.name else { continue };
                let path = Path::new("/dev/input").join(name);
                if !is_event_node(&path) {
                    continue;
                }
                if change.mask.contains(inotify::EventMask::DELETE) {
                    engine.remove(&path);
                } else {
                    engine.add(path).await;
                }
            },
            _ = tick.tick() => engine.tick(),
        }
    }
}

fn is_event_node(path: &Path) -> bool {
    path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("event"))
}

impl Engine {
    fn send(&self, event: Event) {
        let _ = self.events.try_send(event);
    }

    fn now(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    async fn add(&mut self, path: PathBuf) {
        if self.keyboards.contains_key(&path) || self.pointers.contains_key(&path) {
            return;
        }
        let mut dev = match Device::open(&path) {
            Ok(dev) => dev,
            Err(e) => {
                if e.kind() == std::io::ErrorKind::PermissionDenied && self.denied.insert(path) {
                    self.publish_devices();
                }
                return;
            }
        };
        let was_denied = self.denied.remove(&path);
        match classify(&dev) {
            Kind::Keyboard { built_in } => {
                // Grabbing while a key is down (the Enter that launched us) would
                // leave it stuck down in the compositor; wait for release, briefly.
                for _ in 0..40 {
                    if dev.get_key_state().map_or(true, |keys| keys.iter().next().is_none()) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                if dev.grab().is_err() {
                    return;
                }
                let leds: Vec<InputEvent> =
                    self.leds.iter().map(|(&code, &value)| InputEvent::new(EventType::LED.0, code, value)).collect();
                let _ = dev.send_events(&leds);
                let name = dev.name().unwrap_or("Keyboard").split_whitespace().collect::<Vec<_>>().join(" ");
                match dev.into_event_stream() {
                    Ok(stream) => {
                        self.streams.insert(path.clone(), stream);
                        self.keyboards.insert(path, Keyboard { name, built_in, typed: false });
                    }
                    Err(_) => return,
                }
            }
            Kind::BuiltInPointer => {
                if self.pointers_locked {
                    let _ = dev.grab();
                }
                self.pointers.insert(path, dev);
            }
            Kind::Other => {
                if was_denied {
                    self.publish_devices();
                }
                return;
            }
        }
        self.publish_devices();
    }

    fn remove(&mut self, path: &Path) {
        self.streams.remove(path);
        let removed = self.keyboards.remove(path).is_some() | self.pointers.remove(path).is_some();
        if !removed {
            return;
        }
        self.publish_devices();
        if self.lock == Some(LockMode::BuiltIn) && !self.usable_external() {
            self.unlock(UnlockReason::ExternalKeyboardGone);
        }
    }

    fn publish_devices(&self) {
        let mut keyboards: Vec<DeviceInfo> =
            self.keyboards.values().map(|k| DeviceInfo { name: k.name.clone(), built_in: k.built_in }).collect();
        keyboards.sort_by(|a, b| a.name.cmp(&b.name));
        let mut touchpads: Vec<String> =
            self.pointers.values().map(|d| d.name().unwrap_or("Touchpad").trim().to_string()).collect();
        touchpads.sort();
        self.send(Event::Devices { keyboards, touchpads, denied: self.denied.len() });
    }

    fn mirror_leds(&mut self, leds: Vec<(u16, i32)>) {
        for (code, value) in leds {
            self.leds.insert(code, value);
            let ev = InputEvent::new(EventType::LED.0, code, value);
            for (_, stream) in self.streams.iter_mut() {
                let _ = stream.device_mut().send_events(&[ev]);
            }
        }
    }

    fn passes(&self, built_in: bool) -> bool {
        match self.lock {
            None => true,
            Some(LockMode::BuiltIn) => !built_in,
            Some(_) => false,
        }
    }

    fn usable_external(&self) -> bool {
        self.keyboards.values().any(|k| !k.built_in && k.typed)
    }

    fn handle(&mut self, path: &Path, ev: InputEvent) {
        if ev.event_type() == EventType::KEY
            && ev.value() == 1
            && let Some(kb) = self.keyboards.get_mut(path)
        {
            kb.typed = true;
        }
        let built_in = self.keyboards.get(path).is_some_and(|k| k.built_in);
        match ev.event_type() {
            EventType::SYNCHRONIZATION if ev.code() == 0 => {
                if !self.pending.is_empty() {
                    let _ = self.virt.emit(&self.pending);
                    self.pending.clear();
                }
            }
            EventType::MISC if self.passes(built_in) => {
                self.pending.push((uinput::EV_MSC, ev.code(), ev.value()));
            }
            EventType::KEY => self.key(built_in, ev.code(), ev.value()),
            // LED events are our own writes echoed back; forwarding them loops.
            _ => {}
        }
    }

    fn key(&mut self, built_in: bool, code: u16, value: i32) {
        match value {
            1 => {
                self.physical.insert(code);
            }
            0 => {
                self.physical.remove(&code);
            }
            _ => {}
        }
        // The live view mirrors the physical keys, so releases must reach it
        // even when the lock swallows them.
        if self.monitor && value != 2 {
            self.send(Event::Key { code, down: value == 1 });
        }

        if value == 1 && self.recording && !keys::is_modifier(code) {
            if let Some(shortcut) = Shortcut::from_held(&self.physical, code) {
                self.recording = false;
                self.send(Event::ShortcutRecorded(shortcut));
            }
            return;
        }
        if value == 1 && self.cfg.shortcut.matches(&self.physical, code) {
            if self.lock.is_some() {
                self.unlock(UnlockReason::Shortcut);
            } else {
                self.lock(LockReason::Shortcut, self.cfg.mode, self.cfg.lock_touchpad);
            }
            return;
        }
        if !self.passes(built_in) {
            if value == 1 {
                self.send(Event::Blocked);
                if code == K::KEY_ESC.0 {
                    self.send(Event::EscapeWhileLocked);
                }
            }
            return;
        }

        if value != 2
            && self.lock.is_none()
            && let Some(rule) = self.detector.key(code, value == 1, self.now())
        {
            self.lock(LockReason::Smart(rule), self.cfg.mode, self.cfg.lock_touchpad);
            return;
        }

        // Releases and repeats of keys pressed while locked never reached the
        // compositor, so they are dropped too.
        let known = match value {
            1 => self.forwarded.insert(code) || true,
            0 => self.forwarded.remove(&code),
            _ => self.forwarded.contains(&code),
        };
        if known {
            self.pending.push((uinput::EV_KEY, code, value));
        }
    }

    fn tick(&mut self) {
        let now = self.now();
        if self.lock.is_none()
            && let Some(rule) = self.detector.tick(now)
        {
            self.lock(LockReason::Smart(rule), self.cfg.mode, self.cfg.lock_touchpad);
        }
        if self.monitor {
            let chaos = if self.lock.is_some() { 0.0 } else { self.detector.chaos(now) };
            self.send(Event::Chaos(chaos));
        }
    }

    fn command(&mut self, cmd: Command) {
        match cmd {
            Command::Configure(cfg) => {
                self.detector.set_settings(cfg.smart);
                self.cfg = cfg;
            }
            Command::Lock { mode, lock_touchpad } => self.lock(LockReason::Manual, mode, lock_touchpad),
            Command::Unlock(reason) => self.unlock(reason),
            Command::SetTouchpadLocked(on) => self.set_pointers_locked(on),
            Command::Monitor(on) => {
                self.monitor = on;
                // Releases missed while nobody was watching would leave keys lit.
                if on {
                    self.send(Event::KeysHeld(self.physical.iter().copied().collect()));
                }
            }
            Command::RecordShortcut(on) => self.recording = on,
        }
    }

    fn lock(&mut self, reason: LockReason, mode: LockMode, lock_touchpad: bool) {
        if self.lock.is_some() {
            return;
        }
        let has_external = self.usable_external();
        let has_built_in = self.keyboards.values().any(|k| k.built_in);
        // A built-in lock with no external keyboard would leave nothing to type
        // with, so it degrades to a stealth lock of everything.
        let mode = match mode {
            LockMode::BuiltIn if !(has_external && has_built_in) => LockMode::Stealth,
            mode => mode,
        };
        // Release what is held, or the focused app auto-repeats it forever.
        self.pending.clear();
        let release: Vec<_> = self.forwarded.drain().map(|code| (uinput::EV_KEY, code, 0)).collect();
        if !release.is_empty() {
            let _ = self.virt.emit(&release);
        }
        self.lock = Some(mode);
        self.detector.reset();
        if lock_touchpad {
            self.set_pointers_locked(true);
        }
        self.send(Event::Locked { reason, mode, touchpad: self.pointers_locked });
    }

    fn unlock(&mut self, reason: UnlockReason) {
        if self.lock.take().is_none() {
            return;
        }
        self.set_pointers_locked(false);
        self.detector.reset();
        self.send(Event::Unlocked(reason));
    }

    fn set_pointers_locked(&mut self, on: bool) {
        if on == self.pointers_locked {
            return;
        }
        for dev in self.pointers.values_mut() {
            let _ = if on { dev.grab() } else { dev.ungrab() };
        }
        self.pointers_locked = on;
        if self.lock.is_some() {
            self.send(Event::TouchpadLocked(on));
        }
    }
}
