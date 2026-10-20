//! User settings, persisted as TOML under the XDG config directory.

use std::fmt;
use std::path::PathBuf;

use evdev::KeyCode as K;
use serde::{Deserialize, Serialize};

use crate::detect;
use crate::keys;

pub const APP_ID: &str = "io.github.catlock.Catlock";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LockMode {
    /// Every key blocked, lock screen and sounds.
    #[default]
    Full,
    /// Every key blocked, no lock screen and no sounds.
    Stealth,
    /// Only the laptop's own keyboard blocked; external keyboards keep working.
    BuiltIn,
}

impl LockMode {
    pub const ALL: [LockMode; 3] = [LockMode::Full, LockMode::Stealth, LockMode::BuiltIn];

    pub fn label(self) -> &'static str {
        match self {
            LockMode::Full => "Lock screen",
            LockMode::Stealth => "Stealth",
            LockMode::BuiltIn => "Built-in keyboard",
        }
    }
}

/// A modifier chord plus one key, matched against physical key state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shortcut {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub super_: bool,
    pub key: u16,
}

impl Default for Shortcut {
    fn default() -> Self {
        Self { ctrl: true, alt: true, shift: false, super_: false, key: K::KEY_L.0 }
    }
}

impl Shortcut {
    /// Builds a shortcut from the keys currently held, or None when it has no
    /// Ctrl/Alt/Super: a bare key or Shift+key is something a paw can press.
    pub fn from_held(held: &std::collections::HashSet<u16>, key: u16) -> Option<Self> {
        let has = |a: K, b: K| held.contains(&a.0) || held.contains(&b.0);
        let s = Self {
            ctrl: has(K::KEY_LEFTCTRL, K::KEY_RIGHTCTRL),
            alt: has(K::KEY_LEFTALT, K::KEY_RIGHTALT),
            shift: has(K::KEY_LEFTSHIFT, K::KEY_RIGHTSHIFT),
            super_: has(K::KEY_LEFTMETA, K::KEY_RIGHTMETA),
            key,
        };
        (s.ctrl || s.alt || s.super_).then_some(s)
    }

    /// True only when exactly this chord is down: a cat lying on the keyboard
    /// holds the right keys plus many others, which must not count.
    pub fn matches(&self, held: &std::collections::HashSet<u16>, pressed: u16) -> bool {
        pressed == self.key && Self::from_held(held, pressed).as_ref() == Some(self) && {
            let wanted = held.iter().filter(|&&c| c == self.key || keys::is_modifier(c)).count();
            wanted == held.len()
        }
    }

    /// GTK accelerator syntax, for `AdwShortcutLabel`.
    pub fn accelerator(&self) -> String {
        let mut s = String::new();
        for (on, m) in
            [(self.ctrl, "<Control>"), (self.alt, "<Alt>"), (self.shift, "<Shift>"), (self.super_, "<Super>")]
        {
            if on {
                s.push_str(m);
            }
        }
        s + &keys::key_name(self.key).to_lowercase()
    }
}

impl fmt::Display for Shortcut {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        for (on, m) in [(self.ctrl, "Ctrl+"), (self.alt, "Alt+"), (self.shift, "Shift+"), (self.super_, "Super+")] {
            if on {
                f.write_str(m)?;
            }
        }
        f.write_str(&keys::key_name(self.key))
    }
}

impl Serialize for Shortcut {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Shortcut {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        let mut s = Shortcut { ctrl: false, alt: false, shift: false, super_: false, key: 0 };
        for part in text.split('+') {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" => s.ctrl = true,
                "alt" => s.alt = true,
                "shift" => s.shift = true,
                "super" => s.super_ = true,
                key => {
                    s.key = format!("KEY_{}", key.to_ascii_uppercase())
                        .parse::<K>()
                        .map_err(|_| serde::de::Error::custom(format!("unknown key {key:?}")))?
                        .0
                }
            }
        }
        if s.key == 0 || !(s.ctrl || s.alt || s.super_) {
            return Err(serde::de::Error::custom("shortcut needs Ctrl, Alt or Super plus a key"));
        }
        Ok(s)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LockScreen {
    /// A preset from `ui::overlay::BACKGROUNDS`, or "photo".
    pub background: String,
    pub photo: Option<PathBuf>,
    /// Background opacity, 0.1 to 1.0; text and buttons stay fully opaque.
    pub opacity: f32,
    /// An emoji, or empty for no icon.
    pub icon: String,
    /// Empty means the default message.
    pub message: String,
    pub flash: bool,
}

impl Default for LockScreen {
    fn default() -> Self {
        Self {
            background: "tabby".into(),
            photo: None,
            opacity: 1.0,
            icon: "🐱".into(),
            message: String::new(),
            flash: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub name: String,
    pub mode: LockMode,
    pub keep_awake: bool,
    pub lock_touchpad: bool,
    /// A built-in sound id, "silent", or "file:<name>" for an imported clip.
    pub sound: String,
    pub volume: f32,
    pub screen: LockScreen,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            name: "Default".into(),
            mode: LockMode::Full,
            keep_awake: false,
            lock_touchpad: false,
            sound: "warning".into(),
            volume: 0.8,
            screen: LockScreen::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub shortcut: Shortcut,
    pub smart: detect::Settings,
    pub profiles: Vec<Profile>,
    pub active_profile: usize,
}

impl Default for Config {
    fn default() -> Self {
        let stealth =
            Profile { name: "Meeting".into(), mode: LockMode::Stealth, sound: "silent".into(), ..Profile::default() };
        let long_job = Profile { name: "Long job".into(), keep_awake: true, ..Profile::default() };
        Self {
            shortcut: Shortcut::default(),
            smart: detect::Settings::default(),
            profiles: vec![Profile::default(), stealth, long_job],
            active_profile: 0,
        }
    }
}

impl Config {
    pub fn dir() -> PathBuf {
        gtk::glib::user_config_dir().join("catlock")
    }

    fn path() -> PathBuf {
        Self::dir().join("config.toml")
    }

    /// Loads the saved config; a missing or unreadable file falls back to
    /// defaults so a hand-edit mistake never stops the lock from working.
    pub fn load() -> Self {
        let mut cfg = match std::fs::read_to_string(Self::path()) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                eprintln!("catlock: ignoring invalid {}: {e}", Self::path().display());
                Config::default()
            }),
            Err(_) => Config::default(),
        };
        if cfg.profiles.is_empty() {
            cfg.profiles.push(Profile::default());
        }
        cfg.active_profile = cfg.active_profile.min(cfg.profiles.len() - 1);
        cfg
    }

    pub fn save(&self) {
        let result = std::fs::create_dir_all(Self::dir())
            .and_then(|()| std::fs::write(Self::path(), toml::to_string_pretty(self).expect("config serializes")));
        if let Err(e) = result {
            eprintln!("catlock: cannot save {}: {e}", Self::path().display());
        }
    }

    pub fn profile(&self) -> &Profile {
        &self.profiles[self.active_profile]
    }

    pub fn profile_mut(&mut self) -> &mut Profile {
        &mut self.profiles[self.active_profile]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn config_round_trips_through_toml() {
        let cfg = Config::default();
        let text = toml::to_string_pretty(&cfg).unwrap();
        assert!(text.contains("shortcut = \"Ctrl+Alt+L\""));
        assert_eq!(toml::from_str::<Config>(&text).unwrap(), cfg);
    }

    #[test]
    fn shortcut_needs_exactly_its_chord() {
        let s = Shortcut::default();
        let held = |codes: &[K]| codes.iter().map(|k| k.0).collect::<HashSet<_>>();
        assert!(s.matches(&held(&[K::KEY_LEFTCTRL, K::KEY_RIGHTALT, K::KEY_L]), K::KEY_L.0));
        assert!(!s.matches(&held(&[K::KEY_LEFTCTRL, K::KEY_L]), K::KEY_L.0));
        assert!(!s.matches(&held(&[K::KEY_LEFTCTRL, K::KEY_LEFTALT, K::KEY_K, K::KEY_L]), K::KEY_L.0));
        assert!(!s.matches(&held(&[K::KEY_LEFTCTRL, K::KEY_LEFTALT, K::KEY_LEFTSHIFT, K::KEY_L]), K::KEY_L.0));
        assert_eq!(Shortcut::from_held(&held(&[K::KEY_LEFTSHIFT, K::KEY_L]), K::KEY_L.0), None);
    }

    #[test]
    fn partial_config_keeps_defaults() {
        let cfg: Config = toml::from_str("shortcut = \"Super+Shift+F12\"\n[smart]\nsensitivity = 5\n").unwrap();
        assert_eq!(cfg.shortcut.to_string(), "Shift+Super+F12");
        assert_eq!(cfg.smart.sensitivity, 5);
        assert!(cfg.smart.enabled);
        assert_eq!(cfg.profiles.len(), 3);
    }
}
