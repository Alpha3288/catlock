//! Smart Detect: tells a cat on the keyboard from a person using it.
//!
//! Four signals, each with a threshold per sensitivity level:
//! - Paw: several neighbouring keys go down almost together and stay down.
//! - Resting: a few neighbouring keys stay held, as when a cat sits or rests a paw.
//! - Hold: one character key held for seconds, as when a cat stands on it.
//! - Zoomies: a dense burst of presses where keys keep overlapping in chords.
//!
//! People roll over two or three keys while typing fast, but rarely press them
//! within a few milliseconds of each other, rarely on adjacent keys all at once,
//! and almost never with three or more keys held through each new press. Arrow
//! and paging keys are ignored entirely because games and editors hold them in
//! combinations, and presses made while Ctrl/Alt/Super are down are shortcuts.

use std::collections::{HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::keys;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Rule {
    Paw,
    Resting,
    Hold,
    Zoomies,
}

impl Rule {
    pub fn describe(self) -> &'static str {
        match self {
            Rule::Paw => "A paw landed on several keys",
            Rule::Resting => "Something is resting on the keys",
            Rule::Hold => "A key has been held down for a while",
            Rule::Zoomies => "Zoomies across the keyboard",
        }
    }
}

/// Leniency learned from "That wasn't my cat", in sensitivity levels per rule.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Learning {
    pub paw: f32,
    pub resting: f32,
    pub hold: f32,
    pub zoomies: f32,
}

impl Learning {
    const STEP: f32 = 0.5;
    const MAX: f32 = 3.0;

    fn slot(&mut self, rule: Rule) -> &mut f32 {
        match rule {
            Rule::Paw => &mut self.paw,
            Rule::Resting => &mut self.resting,
            Rule::Hold => &mut self.hold,
            Rule::Zoomies => &mut self.zoomies,
        }
    }

    /// A false alarm makes only the rule that fired more tolerant.
    pub fn forgive(&mut self, rule: Rule) {
        let slot = self.slot(rule);
        *slot = (*slot + Self::STEP).min(Self::MAX);
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    /// 1 (lowest) to 5 (highest).
    pub sensitivity: u8,
    pub ignore_single_holds: bool,
    pub learning: Learning,
}

impl Default for Settings {
    fn default() -> Self {
        Self { enabled: true, sensitivity: 3, ignore_single_holds: false, learning: Learning::default() }
    }
}

pub const SENSITIVITY_LABELS: [&str; 5] = ["Lowest", "Low", "Medium", "High", "Highest"];

struct Level {
    paw_keys: usize,
    paw_window_ms: u64,
    rest_keys: usize,
    rest_ms: u64,
    hold_ms: Option<u64>,
    zoom_presses: usize,
}

/// Index 0 is stricter than "Lowest" and only reachable through learning.
const LEVELS: [Level; 6] = [
    Level { paw_keys: 6, paw_window_ms: 50, rest_keys: 5, rest_ms: 1500, hold_ms: None, zoom_presses: 14 },
    Level { paw_keys: 5, paw_window_ms: 60, rest_keys: 4, rest_ms: 1200, hold_ms: None, zoom_presses: 12 },
    Level { paw_keys: 4, paw_window_ms: 80, rest_keys: 4, rest_ms: 1000, hold_ms: None, zoom_presses: 10 },
    Level { paw_keys: 4, paw_window_ms: 100, rest_keys: 3, rest_ms: 800, hold_ms: None, zoom_presses: 8 },
    Level { paw_keys: 3, paw_window_ms: 110, rest_keys: 3, rest_ms: 700, hold_ms: Some(2500), zoom_presses: 7 },
    Level { paw_keys: 3, paw_window_ms: 130, rest_keys: 2, rest_ms: 600, hold_ms: Some(1500), zoom_presses: 6 },
];

/// Largest distance, in key units, between two keys one paw can cover.
const PAW_SPAN: f32 = 3.0;
const ZOOM_WINDOW_MS: u64 = 500;
/// Share of burst presses that must land while two other keys are still held.
const ZOOM_CHORD_SHARE: f32 = 0.3;

struct Press {
    at: u64,
    chord: bool,
}

pub struct Detector {
    settings: Settings,
    /// Counted keys currently down, with the time each went down (ms).
    held: HashMap<u16, u64>,
    shortcut_mods: HashSet<u16>,
    presses: VecDeque<Press>,
}

impl Detector {
    pub fn new(settings: Settings) -> Self {
        Self { settings, held: HashMap::new(), shortcut_mods: HashSet::new(), presses: VecDeque::new() }
    }

    pub fn set_settings(&mut self, settings: Settings) {
        self.settings = settings;
    }

    pub fn reset(&mut self) {
        self.held.clear();
        self.shortcut_mods.clear();
        self.presses.clear();
    }

    fn level(&self, rule: Rule) -> &'static Level {
        let mut learning = self.settings.learning;
        let lenient = *learning.slot(rule);
        let index = (f32::from(self.settings.sensitivity.clamp(1, 5)) - lenient).round();
        &LEVELS[index.clamp(0.0, 5.0) as usize]
    }

    /// Feed a key press (`down`) or release at time `at` (ms). Repeats are not fed.
    pub fn key(&mut self, code: u16, down: bool, at: u64) -> Option<Rule> {
        if keys::is_modifier(code) {
            if keys::is_shortcut_modifier(code) {
                if down {
                    self.shortcut_mods.insert(code);
                } else {
                    self.shortcut_mods.remove(&code);
                }
            }
            return None;
        }
        if keys::is_navigation(code) {
            return None;
        }
        if !down {
            self.held.remove(&code);
            return None;
        }
        if !self.settings.enabled {
            return None;
        }

        let chord = self.held.len() >= 2;
        self.held.insert(code, at);
        if self.shortcut_mods.is_empty() {
            self.presses.push_back(Press { at, chord });
        }
        self.expire(at);

        let paw = self.level(Rule::Paw);
        let fresh: Vec<u16> =
            self.held.iter().filter(|&(_, &t)| at - t <= paw.paw_window_ms).map(|(&c, _)| c).collect();
        if fresh.len() >= paw.paw_keys && compact(&fresh) {
            return Some(Rule::Paw);
        }

        let zoom = self.level(Rule::Zoomies);
        let chords = self.presses.iter().filter(|p| p.chord).count();
        if self.presses.len() >= zoom.zoom_presses && chords as f32 >= self.presses.len() as f32 * ZOOM_CHORD_SHARE {
            return Some(Rule::Zoomies);
        }
        None
    }

    /// Time-based checks; call a few times per second.
    pub fn tick(&mut self, now: u64) -> Option<Rule> {
        if !self.settings.enabled {
            return None;
        }
        let rest = self.level(Rule::Resting);
        let resting: Vec<u16> = self.held.iter().filter(|&(_, &t)| now - t >= rest.rest_ms).map(|(&c, _)| c).collect();
        if resting.len() >= rest.rest_keys && compact(&resting) {
            return Some(Rule::Resting);
        }

        let hold = self.level(Rule::Hold);
        if let (Some(limit), false) = (hold.hold_ms, self.settings.ignore_single_holds)
            && let [(&code, &since)] = self.held.iter().collect::<Vec<_>>()[..]
            && self.shortcut_mods.is_empty()
            && !keys::is_hold_friendly(code)
            && now - since >= limit
        {
            return Some(Rule::Hold);
        }
        None
    }

    /// How close the current activity is to a lock, 0.0 (calm) to 1.0 (cat).
    pub fn chaos(&mut self, now: u64) -> f32 {
        self.expire(now);
        let paw = self.level(Rule::Paw);
        let rest = self.level(Rule::Resting);
        let zoom = self.level(Rule::Zoomies);
        let fresh = self.held.values().filter(|&&t| now - t <= paw.paw_window_ms * 3).count();
        let lingering = self.held.values().filter(|&&t| now - t >= rest.rest_ms / 2).count();
        [
            fresh as f32 / paw.paw_keys as f32,
            lingering as f32 / rest.rest_keys as f32,
            self.presses.len() as f32 / zoom.zoom_presses as f32,
        ]
        .into_iter()
        .fold(0.0_f32, f32::max)
        .min(1.0)
    }

    fn expire(&mut self, now: u64) {
        while self.presses.front().is_some_and(|p| now.saturating_sub(p.at) > ZOOM_WINDOW_MS) {
            self.presses.pop_front();
        }
    }
}

/// True when every pair of keys could sit under one paw. Keys outside the
/// drawn layout (numpad, media) have no position and do not break a cluster.
fn compact(codes: &[u16]) -> bool {
    codes
        .iter()
        .enumerate()
        .all(|(i, &a)| codes[i + 1..].iter().all(|&b| keys::distance(a, b).is_none_or(|d| d <= PAW_SPAN)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use evdev::KeyCode as K;

    fn settings(sensitivity: u8) -> Settings {
        Settings { sensitivity, ..Settings::default() }
    }

    fn code_for(c: char) -> u16 {
        let name = match c {
            ' ' => "KEY_SPACE".to_string(),
            '.' => "KEY_DOT".to_string(),
            ',' => "KEY_COMMA".to_string(),
            c => format!("KEY_{}", c.to_ascii_uppercase()),
        };
        name.parse::<K>().unwrap().0
    }

    /// Replays (time, code, down) events in order, ticking every 10 ms.
    fn replay(d: &mut Detector, mut events: Vec<(u64, u16, bool)>) -> Option<Rule> {
        events.sort_by_key(|e| (e.0, !e.2));
        let end = events.last().map_or(0, |e| e.0) + 3000;
        let mut next = events.into_iter().peekable();
        for now in (0..=end).step_by(10) {
            while let Some(&(at, code, down)) = next.peek() {
                if at > now {
                    break;
                }
                next.next();
                if let Some(rule) = d.key(code, down, at) {
                    return Some(rule);
                }
            }
            if let Some(rule) = d.tick(now) {
                return Some(rule);
            }
        }
        None
    }

    fn typing(text: &str, interval: u64, hold: u64) -> Vec<(u64, u16, bool)> {
        text.chars()
            .enumerate()
            .flat_map(|(i, c)| {
                let at = i as u64 * interval;
                [(at, code_for(c), true), (at + hold, code_for(c), false)]
            })
            .collect()
    }

    const PANGRAM: &str = "the quick brown fox jumps over the lazy dog. pack my box with five dozen liquor jugs, \
                           sphinx of black quartz judge my vow";

    #[test]
    fn steady_typing_is_never_a_cat() {
        for level in 1..=5 {
            let mut d = Detector::new(settings(level));
            assert_eq!(replay(&mut d, typing(PANGRAM, 90, 80)), None, "level {level}");
        }
    }

    #[test]
    fn fast_rollover_typing_passes_at_medium() {
        let mut d = Detector::new(settings(3));
        // About 200 wpm with each key held past the next press.
        assert_eq!(replay(&mut d, typing(PANGRAM, 60, 110)), None);
    }

    #[test]
    fn shortcuts_and_arrow_combos_are_ignored() {
        let mut d = Detector::new(settings(5));
        let ctrl = K::KEY_LEFTCTRL.0;
        let mut events = vec![(0, ctrl, true), (2000, ctrl, false)];
        for (i, k) in [K::KEY_C, K::KEY_V, K::KEY_X, K::KEY_Z].iter().enumerate() {
            let at = 100 + i as u64 * 150;
            events.extend([(at, k.0, true), (at + 60, k.0, false)]);
        }
        for k in [K::KEY_UP, K::KEY_LEFT, K::KEY_DOWN] {
            events.extend([(2500, k.0, true), (4500, k.0, false)]);
        }
        assert_eq!(replay(&mut d, events), None);
    }

    #[test]
    fn a_paw_landing_locks() {
        let mut d = Detector::new(settings(3));
        let paw = [K::KEY_F, K::KEY_G, K::KEY_V, K::KEY_B];
        let events =
            paw.iter().enumerate().flat_map(|(i, k)| [(i as u64 * 15, k.0, true), (400, k.0, false)]).collect();
        assert_eq!(replay(&mut d, events), Some(Rule::Paw));
    }

    #[test]
    fn a_resting_paw_locks_even_when_it_lands_slowly() {
        let mut d = Detector::new(settings(3));
        let events = [K::KEY_J, K::KEY_K, K::KEY_M]
            .iter()
            .enumerate()
            .flat_map(|(i, k)| [(i as u64 * 200, k.0, true), (2500, k.0, false)])
            .collect();
        assert_eq!(replay(&mut d, events), Some(Rule::Resting));
    }

    #[test]
    fn single_key_holds_respect_level_and_opt_out() {
        let held_k = vec![(0, K::KEY_K.0, true), (4000, K::KEY_K.0, false)];
        assert_eq!(replay(&mut Detector::new(settings(3)), held_k.clone()), None);
        assert_eq!(replay(&mut Detector::new(settings(5)), held_k.clone()), Some(Rule::Hold));
        let opted_out = Settings { ignore_single_holds: true, ..settings(5) };
        assert_eq!(replay(&mut Detector::new(opted_out), held_k), None);
        let backspace = vec![(0, K::KEY_BACKSPACE.0, true), (4000, K::KEY_BACKSPACE.0, false)];
        assert_eq!(replay(&mut Detector::new(settings(5)), backspace), None);
    }

    #[test]
    fn zoomies_lock() {
        let mut d = Detector::new(settings(3));
        // Four paws pattering across the board, each step overlapping the last two.
        let path = "qawsedrftgyhujikolp";
        let events = path
            .chars()
            .enumerate()
            .flat_map(|(i, c)| {
                let at = i as u64 * 35;
                [(at, code_for(c), true), (at + 90, code_for(c), false)]
            })
            .collect();
        assert_eq!(replay(&mut d, events), Some(Rule::Zoomies));
    }

    #[test]
    fn forgiving_a_rule_makes_it_stricter() {
        let paw3 = [K::KEY_F, K::KEY_G, K::KEY_V]
            .iter()
            .enumerate()
            .flat_map(|(i, k)| [(i as u64 * 20, k.0, true), (300, k.0, false)])
            .collect::<Vec<_>>();
        let mut s = settings(4);
        assert_eq!(replay(&mut Detector::new(s), paw3.clone()), Some(Rule::Paw));
        s.learning.forgive(Rule::Paw);
        s.learning.forgive(Rule::Paw);
        assert_eq!(replay(&mut Detector::new(s), paw3), None);
    }

    #[test]
    fn disabled_detector_never_fires() {
        let mut d = Detector::new(Settings { enabled: false, ..settings(5) });
        let events = [K::KEY_F, K::KEY_G, K::KEY_V, K::KEY_B]
            .iter()
            .flat_map(|k| [(0, k.0, true), (5000, k.0, false)])
            .collect();
        assert_eq!(replay(&mut d, events), None);
    }
}
