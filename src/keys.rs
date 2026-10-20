//! Physical keyboard geometry and key classes.
//!
//! Smart Detect reasons about where keys sit (a paw covers neighbours, a human
//! rollover rarely does) and the heat map draws the same layout, so both read
//! from this one table. Coordinates are in key units on an ANSI layout; ISO
//! and other physical layouts differ by a key or so, which the detector's
//! distance thresholds tolerate.

use std::collections::HashMap;
use std::sync::OnceLock;

use evdev::KeyCode as K;

#[derive(Clone, Copy, Debug)]
pub struct KeyCap {
    pub code: u16,
    pub x: f32,
    pub y: f32,
    pub w: f32,
}

/// Width of the drawn layout in key units (main block plus navigation cluster).
pub const LAYOUT_WIDTH: f32 = 18.5;
pub const LAYOUT_HEIGHT: f32 = 6.0;

pub fn layout() -> &'static [KeyCap] {
    static LAYOUT: OnceLock<Vec<KeyCap>> = OnceLock::new();
    LAYOUT.get_or_init(build_layout)
}

/// Centre of a key, when it is part of the drawn layout.
pub fn position(code: u16) -> Option<(f32, f32)> {
    static INDEX: OnceLock<HashMap<u16, (f32, f32)>> = OnceLock::new();
    INDEX.get_or_init(|| layout().iter().map(|k| (k.code, (k.x + k.w / 2.0, k.y + 0.5))).collect()).get(&code).copied()
}

pub fn distance(a: u16, b: u16) -> Option<f32> {
    let (ax, ay) = position(a)?;
    let (bx, by) = position(b)?;
    Some(((ax - bx).powi(2) + (ay - by).powi(2)).sqrt())
}

pub fn is_modifier(code: u16) -> bool {
    matches!(
        K(code),
        K::KEY_LEFTSHIFT
            | K::KEY_RIGHTSHIFT
            | K::KEY_LEFTCTRL
            | K::KEY_RIGHTCTRL
            | K::KEY_LEFTALT
            | K::KEY_RIGHTALT
            | K::KEY_LEFTMETA
            | K::KEY_RIGHTMETA
    )
}

/// Modifiers that turn a keypress into a shortcut (Shift alone only changes case).
pub fn is_shortcut_modifier(code: u16) -> bool {
    is_modifier(code) && !matches!(K(code), K::KEY_LEFTSHIFT | K::KEY_RIGHTSHIFT)
}

/// Keys people legitimately hold down or hammer: editing, navigation, and the
/// characters used to draw dividers. Holding one of these is never a cat signal.
pub fn is_hold_friendly(code: u16) -> bool {
    matches!(
        K(code),
        K::KEY_BACKSPACE
            | K::KEY_DELETE
            | K::KEY_SPACE
            | K::KEY_ENTER
            | K::KEY_KPENTER
            | K::KEY_TAB
            | K::KEY_ESC
            | K::KEY_UP
            | K::KEY_DOWN
            | K::KEY_LEFT
            | K::KEY_RIGHT
            | K::KEY_HOME
            | K::KEY_END
            | K::KEY_PAGEUP
            | K::KEY_PAGEDOWN
            | K::KEY_MINUS
            | K::KEY_EQUAL
            | K::KEY_DOT
            | K::KEY_COMMA
            | K::KEY_SLASH
            | K::KEY_KPMINUS
            | K::KEY_KPPLUS
    )
}

/// Arrow and paging keys: games and editors hold several at once.
pub fn is_navigation(code: u16) -> bool {
    matches!(
        K(code),
        K::KEY_UP
            | K::KEY_DOWN
            | K::KEY_LEFT
            | K::KEY_RIGHT
            | K::KEY_HOME
            | K::KEY_END
            | K::KEY_PAGEUP
            | K::KEY_PAGEDOWN
    )
}

/// Human-readable key name, e.g. `L`, `F5`, `Space`.
pub fn key_name(code: u16) -> String {
    let raw = format!("{:?}", K(code));
    let name = raw.strip_prefix("KEY_").unwrap_or(&raw);
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_string() + &chars.as_str().to_lowercase(),
        None => raw,
    }
}

fn build_layout() -> Vec<KeyCap> {
    let mut caps = Vec::new();
    let mut row = |y: f32, start: f32, keys: &[(K, f32)]| {
        let mut x = start;
        for &(code, w) in keys {
            if code != K::KEY_RESERVED {
                caps.push(KeyCap { code: code.0, x, y, w });
            }
            x += w;
        }
    };
    let gap = |w: f32| (K::KEY_RESERVED, w);
    let one = |codes: &[K]| codes.iter().map(|&c| (c, 1.0)).collect::<Vec<_>>();

    let mut f_row = vec![(K::KEY_ESC, 1.0), gap(1.0)];
    f_row.extend(one(&[K::KEY_F1, K::KEY_F2, K::KEY_F3, K::KEY_F4]));
    f_row.push(gap(0.5));
    f_row.extend(one(&[K::KEY_F5, K::KEY_F6, K::KEY_F7, K::KEY_F8]));
    f_row.push(gap(0.5));
    f_row.extend(one(&[K::KEY_F9, K::KEY_F10, K::KEY_F11, K::KEY_F12]));
    row(0.0, 0.0, &f_row);

    let mut numbers = one(&[
        K::KEY_GRAVE,
        K::KEY_1,
        K::KEY_2,
        K::KEY_3,
        K::KEY_4,
        K::KEY_5,
        K::KEY_6,
        K::KEY_7,
        K::KEY_8,
        K::KEY_9,
        K::KEY_0,
        K::KEY_MINUS,
        K::KEY_EQUAL,
    ]);
    numbers.push((K::KEY_BACKSPACE, 2.0));
    row(1.0, 0.0, &numbers);

    let mut top = vec![(K::KEY_TAB, 1.5)];
    top.extend(one(&[
        K::KEY_Q,
        K::KEY_W,
        K::KEY_E,
        K::KEY_R,
        K::KEY_T,
        K::KEY_Y,
        K::KEY_U,
        K::KEY_I,
        K::KEY_O,
        K::KEY_P,
        K::KEY_LEFTBRACE,
        K::KEY_RIGHTBRACE,
    ]));
    top.push((K::KEY_BACKSLASH, 1.5));
    row(2.0, 0.0, &top);

    let mut home = vec![(K::KEY_CAPSLOCK, 1.75)];
    home.extend(one(&[
        K::KEY_A,
        K::KEY_S,
        K::KEY_D,
        K::KEY_F,
        K::KEY_G,
        K::KEY_H,
        K::KEY_J,
        K::KEY_K,
        K::KEY_L,
        K::KEY_SEMICOLON,
        K::KEY_APOSTROPHE,
    ]));
    home.push((K::KEY_ENTER, 2.25));
    row(3.0, 0.0, &home);

    let mut bottom = vec![(K::KEY_LEFTSHIFT, 2.25)];
    bottom.extend(one(&[
        K::KEY_Z,
        K::KEY_X,
        K::KEY_C,
        K::KEY_V,
        K::KEY_B,
        K::KEY_N,
        K::KEY_M,
        K::KEY_COMMA,
        K::KEY_DOT,
        K::KEY_SLASH,
    ]));
    bottom.push((K::KEY_RIGHTSHIFT, 2.75));
    row(4.0, 0.0, &bottom);

    row(
        5.0,
        0.0,
        &[
            (K::KEY_LEFTCTRL, 1.25),
            (K::KEY_LEFTMETA, 1.25),
            (K::KEY_LEFTALT, 1.25),
            (K::KEY_SPACE, 6.25),
            (K::KEY_RIGHTALT, 1.25),
            (K::KEY_RIGHTMETA, 1.25),
            (K::KEY_COMPOSE, 1.25),
            (K::KEY_RIGHTCTRL, 1.25),
        ],
    );

    row(1.0, 15.5, &one(&[K::KEY_INSERT, K::KEY_HOME, K::KEY_PAGEUP]));
    row(2.0, 15.5, &one(&[K::KEY_DELETE, K::KEY_END, K::KEY_PAGEDOWN]));
    row(4.0, 16.5, &one(&[K::KEY_UP]));
    row(5.0, 15.5, &one(&[K::KEY_LEFT, K::KEY_DOWN, K::KEY_RIGHT]));
    caps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neighbours_are_close_and_far_keys_are_not() {
        let d = |a: K, b: K| distance(a.0, b.0).unwrap();
        assert!(d(K::KEY_F, K::KEY_G) < 1.1);
        assert!(d(K::KEY_F, K::KEY_V) < 1.2);
        assert!(d(K::KEY_Q, K::KEY_P) > 8.0);
    }

    #[test]
    fn names_read_naturally() {
        assert_eq!(key_name(K::KEY_L.0), "L");
        assert_eq!(key_name(K::KEY_SPACE.0), "Space");
        assert_eq!(key_name(K::KEY_F5.0), "F5");
    }
}
