//! Live keyboard heat map for the Smart Detect page: shows what the detector
//! sees, so sensitivity can be tuned against real typing and real paws.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{cairo, gdk};

use crate::keys;

#[derive(Default)]
struct Heat {
    level: f32,
    held: bool,
}

#[derive(Clone)]
pub struct HeatMap {
    pub widget: gtk::DrawingArea,
    heat: Rc<RefCell<HashMap<u16, Heat>>>,
}

/// Fraction of heat a released key keeps after one second.
const COOL_PER_SECOND: f64 = 0.08;

impl HeatMap {
    pub fn new() -> Self {
        let widget = gtk::DrawingArea::builder().content_height(190).hexpand(true).build();
        let heat: Rc<RefCell<HashMap<u16, Heat>>> = Rc::default();

        let state = heat.clone();
        widget.set_draw_func(move |area, cr, w, h| draw(area, cr, w as f64, h as f64, &state.borrow()));

        let state = heat.clone();
        let last = std::cell::Cell::new(None::<i64>);
        widget.add_tick_callback(move |area, clock| {
            let now = clock.frame_time();
            let dt = last.replace(Some(now)).map_or(0.0, |t| (now - t) as f64 / 1_000_000.0);
            let keep = COOL_PER_SECOND.powf(dt) as f32;
            let mut changed = false;
            for h in state.borrow_mut().values_mut() {
                if !h.held && h.level > 0.01 {
                    h.level *= keep;
                    changed = true;
                }
            }
            if changed {
                area.queue_draw();
            }
            gtk::glib::ControlFlow::Continue
        });
        Self { widget, heat }
    }

    /// Makes exactly `held` the keys shown as down; the rest cool off.
    pub fn sync(&self, held: &[u16]) {
        let mut heat = self.heat.borrow_mut();
        for (code, h) in heat.iter_mut() {
            h.held = held.contains(code);
        }
        for &code in held {
            heat.entry(code).or_insert(Heat { level: 1.0, held: true });
        }
        drop(heat);
        self.widget.queue_draw();
    }

    pub fn key(&self, code: u16, down: bool) {
        let mut heat = self.heat.borrow_mut();
        let entry = heat.entry(code).or_default();
        entry.held = down;
        if down {
            entry.level = 1.0;
        }
        drop(heat);
        self.widget.queue_draw();
    }
}

fn rounded(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    use std::f64::consts::{FRAC_PI_2, PI};
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -FRAC_PI_2, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, FRAC_PI_2);
    cr.arc(x + r, y + h - r, r, FRAC_PI_2, PI);
    cr.arc(x + r, y + r, r, PI, 3.0 * FRAC_PI_2);
    cr.close_path();
}

fn draw(area: &gtk::DrawingArea, cr: &cairo::Context, w: f64, h: f64, heat: &HashMap<u16, Heat>) {
    let unit = (w / keys::LAYOUT_WIDTH as f64).min(h / keys::LAYOUT_HEIGHT as f64);
    let ox = (w - unit * keys::LAYOUT_WIDTH as f64) / 2.0;
    let oy = (h - unit * keys::LAYOUT_HEIGHT as f64) / 2.0;
    let gap = (unit * 0.12).max(2.0);
    let fg = area.color();
    let hot = gdk::RGBA::new(0.94, 0.30, 0.24, 1.0);

    for cap in keys::layout() {
        let level = heat.get(&cap.code).map_or(0.0, |h| h.level) as f64;
        let (x, y) = (ox + cap.x as f64 * unit, oy + cap.y as f64 * unit);
        rounded(cr, x + gap / 2.0, y + gap / 2.0, cap.w as f64 * unit - gap, unit - gap, unit * 0.18);
        // Idle keys are a faint tint of the text colour so they work in light and dark.
        let idle_alpha = 0.10;
        let mix = |a: f32, b: f32| a as f64 * (1.0 - level) + b as f64 * level;
        cr.set_source_rgba(
            mix(fg.red(), hot.red()),
            mix(fg.green(), hot.green()),
            mix(fg.blue(), hot.blue()),
            idle_alpha + (0.95 - idle_alpha) * level,
        );
        let _ = cr.fill();
    }
}
