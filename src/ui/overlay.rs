//! The full-screen lock screen, one window per monitor.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib};

use crate::config::{LockScreen, Shortcut};
use crate::detect::Rule;

/// Lock screen backgrounds: id, label. Each id has a `.bg-<id>` CSS class.
pub const BACKGROUNDS: &[(&str, &str)] = &[
    ("tabby", "Tabby"),
    ("prism", "Prism"),
    ("sage", "Sage"),
    ("midnight", "Midnight"),
    ("cream", "Cream"),
    ("dim", "Dimmed"),
];

pub const ICONS: &[(&str, &str)] = &[
    ("🐾", "Paw"),
    ("🐱", "Cat"),
    ("😼", "Smirk"),
    ("🍼", "Bottle"),
    ("🫧", "Bubbles"),
    ("🧹", "Broom"),
    ("🐟", "Fish"),
];

pub const DEFAULT_MESSAGE: &str = "Keyboard locked";

/// Below this the lock screen is too faint to notice that the keyboard is locked.
pub const MIN_OPACITY: f32 = 0.1;

pub struct Overlay {
    windows: Vec<gtk::Window>,
    frames: Vec<gtk::Box>,
    photo_css: Option<gtk::CssProvider>,
}

pub enum Purpose {
    Lock {
        smart: Option<Rule>,
        on_not_my_cat: Rc<dyn Fn()>,
    },
    /// Shown from settings; any click closes it.
    Preview,
}

impl Overlay {
    pub fn show(
        app: &adw::Application,
        screen: &LockScreen,
        shortcut: &Shortcut,
        purpose: Purpose,
    ) -> Rc<RefCell<Option<Overlay>>> {
        let display = gdk::Display::default().expect("display");
        let photo_css = photo_provider(&display, screen);
        let background = if photo_css.is_some() { "photo" } else { screen.background.as_str() };

        let slot: Rc<RefCell<Option<Overlay>>> = Rc::new(RefCell::new(None));
        let mut windows = Vec::new();
        let mut frames = Vec::new();
        let monitors = display.monitors();
        // GNOME Shell draws nothing behind a fullscreen window, so a translucent
        // lock screen must be an ordinary maximized window instead. Only
        // fullscreen can target a monitor, so a translucent lock screen is a
        // single window on the monitor GNOME picks (the one in use).
        let translucent = screen.opacity < 1.0;
        let count = if translucent { monitors.n_items().min(1) } else { monitors.n_items() };
        for i in 0..count {
            let Some(monitor) = monitors.item(i).and_downcast::<gdk::Monitor>() else { continue };
            let frame = content(screen, shortcut, &purpose, &slot);
            // The background sits on its own layer so transparency fades it
            // without fading the message and buttons drawn above it.
            let backdrop = gtk::Box::builder().css_classes(["overlay-bg", &format!("bg-{background}")]).build();
            backdrop.set_opacity(f64::from(screen.opacity.clamp(MIN_OPACITY, 1.0)));
            let layers = gtk::Overlay::builder().child(&backdrop).build();
            layers.add_overlay(&frame);
            let mut classes = vec!["catlock-overlay"];
            if background == "cream" {
                classes.push("on-light");
            }
            let window = gtk::Window::builder()
                .application(app)
                .decorated(false)
                .title("Catlock")
                .child(&layers)
                .css_classes(classes)
                .build();
            if matches!(purpose, Purpose::Preview) {
                let click = gtk::GestureClick::new();
                let slot = slot.clone();
                click.connect_released(move |_, _, _, _| close(&slot));
                window.add_controller(click);
            }
            if translucent {
                window.maximize();
            } else {
                window.fullscreen_on_monitor(&monitor);
            }
            window.present();
            windows.push(window);
            frames.push(frame);
        }
        *slot.borrow_mut() = Some(Overlay { windows, frames, photo_css });
        if matches!(purpose, Purpose::Preview) {
            let slot = slot.clone();
            glib::timeout_add_local_once(std::time::Duration::from_secs(6), move || close(&slot));
        }
        slot
    }

    /// Briefly rings the screen edge to show a keystroke was blocked.
    pub fn flash(&self) {
        for frame in &self.frames {
            frame.add_css_class("flash");
            let frame = frame.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(140), move || {
                frame.remove_css_class("flash")
            });
        }
    }

    pub fn close(self) {
        for window in self.windows {
            window.destroy();
        }
        if let (Some(css), Some(display)) = (self.photo_css, gdk::Display::default()) {
            gtk::style_context_remove_provider_for_display(&display, &css);
        }
    }
}

pub fn close(slot: &Rc<RefCell<Option<Overlay>>>) {
    if let Some(overlay) = slot.borrow_mut().take() {
        overlay.close();
    }
}

fn photo_provider(display: &gdk::Display, screen: &LockScreen) -> Option<gtk::CssProvider> {
    if screen.background != "photo" {
        return None;
    }
    let path = screen.photo.as_ref().filter(|p| p.exists())?;
    let uri = gtk::gio::File::for_path(path).uri();
    let css = gtk::CssProvider::new();
    css.load_from_string(&format!(
        ".overlay-bg.bg-photo {{ background-image: linear-gradient(rgba(0,0,0,0.35), rgba(0,0,0,0.55)), \
         url(\"{uri}\"); background-size: cover; background-position: center; }}"
    ));
    gtk::style_context_add_provider_for_display(display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1);
    Some(css)
}

fn content(
    screen: &LockScreen,
    shortcut: &Shortcut,
    purpose: &Purpose,
    slot: &Rc<RefCell<Option<Overlay>>>,
) -> gtk::Box {
    let frame = gtk::Box::builder().orientation(gtk::Orientation::Vertical).css_classes(["overlay-frame"]).build();
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .valign(gtk::Align::Center)
        .vexpand(true)
        .halign(gtk::Align::Center)
        .build();
    frame.append(&column);

    if !screen.icon.is_empty() {
        column.append(&gtk::Label::builder().label(&screen.icon).css_classes(["overlay-icon"]).build());
    }
    let message = if screen.message.trim().is_empty() { DEFAULT_MESSAGE } else { screen.message.trim() };
    column.append(
        &gtk::Label::builder()
            .label(message)
            .wrap(true)
            .justify(gtk::Justification::Center)
            .css_classes(["overlay-title"])
            .build(),
    );

    let hint = match purpose {
        Purpose::Preview => "Preview. Click anywhere to close.".to_string(),
        Purpose::Lock { .. } => format!("Press {shortcut} to unlock"),
    };
    column.append(&gtk::Label::builder().label(&hint).css_classes(["overlay-hint"]).build());

    if let Purpose::Lock { smart: Some(rule), on_not_my_cat } = purpose {
        column.append(&gtk::Label::builder().label(rule.describe()).css_classes(["overlay-reason"]).build());
        let button = gtk::Button::builder()
            .label("That wasn't my cat")
            .halign(gtk::Align::Center)
            .margin_top(12)
            .css_classes(["pill", "overlay-button"])
            .build();
        let on_not_my_cat = on_not_my_cat.clone();
        let slot = slot.clone();
        button.connect_clicked(move |_| {
            close(&slot);
            on_not_my_cat();
        });
        column.append(&button);
    }
    frame
}
