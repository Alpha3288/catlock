//! The settings window: a sidebar of pages, each rebuilt from the config.
//!
//! Pages are rebuilt rather than patched in place whenever state they show
//! changes from outside (profile switch, tray toggle, recorded shortcut), so
//! there is no widget-sync code to drift out of date.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use super::heatmap::HeatMap;
use super::overlay::{BACKGROUNDS, DEFAULT_MESSAGE, ICONS};
use super::{Controller, LockState};
use crate::config::{APP_ID, LockMode, Profile};
use crate::detect::SENSITIVITY_LABELS;
use crate::engine::{Command, LockReason};
use crate::{sound, system};

const PAGES: &[(&str, &str, &str)] = &[
    ("home", "Home", "user-home-symbolic"),
    ("smart", "Smart Detect", "star-new-symbolic"),
    ("lock", "Lock Settings", "changes-prevent-symbolic"),
    ("screen", "Lock Screen", "video-display-symbolic"),
    ("sound", "Sound", "audio-volume-high-symbolic"),
    ("general", "General", "preferences-system-symbolic"),
];

#[derive(Clone)]
pub struct Window(Rc<Inner>);

struct Inner {
    ctl: Rc<Controller>,
    window: adw::ApplicationWindow,
    stack: gtk::Stack,
    toasts: adw::ToastOverlay,
    heatmap: RefCell<Option<HeatMap>>,
    chaos: RefCell<Option<(gtk::Image, gtk::Label)>>,
    recording: Cell<bool>,
    reload_queued: Cell<bool>,
}

fn group(title: &str, description: &str) -> adw::PreferencesGroup {
    let g = adw::PreferencesGroup::builder().title(title).build();
    if !description.is_empty() {
        g.set_description(Some(description));
    }
    g
}

fn sound_label(id: &str) -> String {
    if id == sound::SILENT {
        return "Silent".into();
    }
    sound::custom_name(id)
        .map(str::to_string)
        .or_else(|| sound::BUILTIN.iter().find(|b| b.id == id).map(|b| b.label.to_string()))
        .unwrap_or_else(|| id.to_string())
}

impl Window {
    pub fn new(ctl: &Rc<Controller>) -> Self {
        let stack = gtk::Stack::builder().transition_type(gtk::StackTransitionType::Crossfade).build();
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&stack));

        let content_bar = adw::ToolbarView::new();
        content_bar.add_top_bar(&adw::HeaderBar::new());
        content_bar.set_content(Some(&toasts));
        let content = adw::NavigationPage::builder().title(PAGES[0].1).tag("content").child(&content_bar).build();

        let sidebar = adw::Sidebar::new();
        let section = adw::SidebarSection::new();
        for (_, title, icon) in PAGES {
            section.append(adw::SidebarItem::builder().title(*title).icon_name(*icon).build());
        }
        sidebar.append(section);

        let sidebar_bar = adw::ToolbarView::new();
        sidebar_bar.add_top_bar(&adw::HeaderBar::new());
        sidebar_bar.set_content(Some(&sidebar));
        let footer = gtk::Label::builder()
            .label(format!("Catlock {}", env!("CARGO_PKG_VERSION")))
            .css_classes(["dim-label", "caption"])
            .margin_bottom(12)
            .margin_top(6)
            .build();
        sidebar_bar.add_bottom_bar(&footer);
        let sidebar_page = adw::NavigationPage::builder().title("Catlock").child(&sidebar_bar).build();

        let split = adw::NavigationSplitView::builder()
            .sidebar(&sidebar_page)
            .content(&content)
            .min_sidebar_width(220.0)
            .build();

        let window = adw::ApplicationWindow::builder()
            .application(&ctl.app)
            .title("Catlock")
            .default_width(940)
            .default_height(680)
            .hide_on_close(true)
            .content(&split)
            .build();
        let narrow =
            adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 640sp").expect("valid condition"));
        narrow.add_setter(&split, "collapsed", Some(&true.to_value()));
        window.add_breakpoint(narrow);

        let this = Self(Rc::new(Inner {
            ctl: ctl.clone(),
            window,
            stack,
            toasts,
            heatmap: RefCell::new(None),
            chaos: RefCell::new(None),
            recording: Cell::new(false),
            reload_queued: Cell::new(false),
        }));
        for (name, _, _) in PAGES {
            this.rebuild(name);
        }

        let w = this.clone();
        sidebar.connect_activated(move |_, index| {
            let (name, title, _) = PAGES[index as usize];
            w.0.stack.set_visible_child_name(name);
            content.set_title(title);
            split.set_show_content(true);
            w.update_monitor();
        });
        let w = this.clone();
        this.0.window.connect_visible_notify(move |_| w.update_monitor());
        this
    }

    /// Debug builds only: renders every page, then a lock screen preview, to
    /// PNG files so the UI can be checked in a headless compositor.
    #[cfg(debug_assertions)]
    pub fn snapshot_pages(&self, dir: std::path::PathBuf) {
        let w = self.clone();
        let step = Rc::new(Cell::new(0usize));
        glib::timeout_add_local(std::time::Duration::from_millis(900), move || {
            let i = step.get();
            step.set(i + 1);
            if i > 0 && i <= PAGES.len() {
                save_png(w.0.window.upcast_ref(), &dir.join(format!("{}-{}.png", i, PAGES[i - 1].0)));
            }
            if i < PAGES.len() {
                w.0.stack.set_visible_child_name(PAGES[i].0);
                return glib::ControlFlow::Continue;
            }
            if i == PAGES.len() {
                w.0.ctl.preview_lock_screen();
                return glib::ControlFlow::Continue;
            }
            for top in gtk::Window::list_toplevels() {
                if let Ok(win) = top.downcast::<gtk::Window>()
                    && win.has_css_class("catlock-overlay")
                {
                    save_png(&win, &dir.join("7-lock-screen.png"));
                }
            }
            w.0.ctl.app.quit();
            glib::ControlFlow::Break
        });
    }

    pub fn present(&self) {
        self.0.window.present();
        self.update_monitor();
    }

    fn update_monitor(&self) {
        let watching = self.0.window.is_visible() && self.0.stack.visible_child_name().as_deref() == Some("smart");
        self.0.ctl.set_monitor(watching);
    }

    fn toast(&self, text: &str) {
        self.0.toasts.add_toast(adw::Toast::new(text));
    }

    /// Rebuilds every page on the next idle, after the signal handler that
    /// asked for it has returned (it may live on a page being replaced).
    pub fn reload(&self) {
        if self.0.reload_queued.replace(true) {
            return;
        }
        let w = self.clone();
        glib::idle_add_local_once(move || {
            w.0.reload_queued.set(false);
            for (name, _, _) in PAGES {
                w.rebuild(name);
            }
        });
    }

    pub fn shortcut_recorded(&self) {
        self.0.recording.set(false);
        self.reload();
    }

    pub fn refresh_status(&self) {
        let w = self.clone();
        glib::idle_add_local_once(move || {
            w.rebuild("home");
            w.rebuild("general");
        });
    }

    pub fn key(&self, code: u16, down: bool) {
        if let Some(map) = self.0.heatmap.borrow().as_ref() {
            map.key(code, down);
        }
    }

    pub fn chaos(&self, level: f32) {
        let Some((icon, label)) = self.0.chaos.borrow().clone() else { return };
        let (text, class, icon_name) = match level {
            l if l >= 0.75 => ("Keyboard chaos detected", "error", "dialog-warning-symbolic"),
            l if l >= 0.4 => ("Something is stirring", "warning", "emoji-nature-symbolic"),
            _ => ("All quiet", "success", "object-select-symbolic"),
        };
        label.set_label(text);
        icon.set_icon_name(Some(icon_name));
        for c in ["error", "warning", "success"] {
            label.remove_css_class(c);
            icon.remove_css_class(c);
        }
        label.add_css_class(class);
        icon.add_css_class(class);
    }

    fn rebuild(&self, name: &str) {
        let page: gtk::Widget = match name {
            "home" => self.home_page().upcast(),
            "smart" => self.smart_page().upcast(),
            "lock" => self.lock_page().upcast(),
            "screen" => self.screen_page().upcast(),
            "sound" => self.sound_page().upcast(),
            _ => self.general_page().upcast(),
        };
        let stack = &self.0.stack;
        let visible = stack.visible_child_name();
        if let Some(old) = stack.child_by_name(name) {
            stack.remove(&old);
        }
        stack.add_named(&page, Some(name));
        if let Some(visible) = visible {
            stack.set_visible_child_name(&visible);
        }
    }

    fn ctl(&self) -> Rc<Controller> {
        self.0.ctl.clone()
    }

    /// Applies a change to the active profile.
    fn edit_profile(&self, f: impl FnOnce(&mut Profile)) {
        self.0.ctl.edit(|c| f(c.profile_mut()));
    }

    fn home_page(&self) -> adw::PreferencesPage {
        let ctl = self.ctl();
        let page = adw::PreferencesPage::new();
        let cfg = ctl.cfg.borrow();
        let lock = ctl.lock.get();

        if let Some(message) = ctl.failure.borrow().as_ref() {
            let g = group("Catlock is not running", "");
            g.add(&adw::Banner::builder().title(message).revealed(true).build());
            g.add(&permission_help());
            page.add(&g);
        }

        let hero = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .halign(gtk::Align::Center)
            .margin_top(12)
            .margin_bottom(12)
            .build();
        hero.append(
            &gtk::Label::builder().label(if lock.is_some() { "😼" } else { "🐱" }).css_classes(["hero-icon"]).build(),
        );
        let title = match lock {
            Some(LockState { mode: LockMode::BuiltIn, .. }) => "Built-in keyboard locked",
            Some(_) => "Keyboard locked",
            None => "Keyboard unlocked",
        };
        hero.append(&gtk::Label::builder().label(title).css_classes(["title-1"]).build());
        let subtitle = match lock {
            Some(LockState { reason: LockReason::Smart(rule), .. }) => {
                format!("{}. Press {} to unlock.", rule.describe(), cfg.shortcut)
            }
            Some(_) => format!("Press {} anywhere to unlock.", cfg.shortcut),
            None => format!("Press {} anywhere to lock, or let Smart Detect do it.", cfg.shortcut),
        };
        hero.append(
            &gtk::Label::builder()
                .label(subtitle)
                .wrap(true)
                .justify(gtk::Justification::Center)
                .css_classes(["dim-label"])
                .build(),
        );
        let button = gtk::Button::builder()
            .label(if lock.is_some() { "Unlock Keyboard" } else { "Lock Keyboard" })
            .halign(gtk::Align::Center)
            .margin_top(12)
            .css_classes(["pill", "suggested-action"])
            .build();
        let c = ctl.clone();
        button.connect_clicked(move |_| c.toggle());
        hero.append(&button);
        let hero_group = adw::PreferencesGroup::new();
        hero_group.add(&hero);
        page.add(&hero_group);

        let profiles = group(
            "Profiles",
            "Each profile keeps its own lock mode, sound and lock screen. Start one from the tray menu.",
        );
        let mut first: Option<gtk::CheckButton> = None;
        for (i, profile) in cfg.profiles.iter().enumerate() {
            let row = adw::ActionRow::builder()
                .title(&profile.name)
                .subtitle(format!("{} · {}", profile.mode.label(), sound_label(&profile.sound)))
                .build();
            let check = gtk::CheckButton::builder().active(i == cfg.active_profile).valign(gtk::Align::Center).build();
            check.set_group(first.as_ref());
            first.get_or_insert(check.clone());
            let w = self.clone();
            check.connect_toggled(move |b| {
                if b.is_active() && w.0.ctl.cfg.borrow().active_profile != i {
                    w.0.ctl.edit(|c| c.active_profile = i);
                    w.reload();
                }
            });
            row.add_prefix(&check);
            row.set_activatable_widget(Some(&check));
            if cfg.profiles.len() > 1 {
                let delete = gtk::Button::builder()
                    .icon_name("user-trash-symbolic")
                    .valign(gtk::Align::Center)
                    .tooltip_text("Delete profile")
                    .css_classes(["flat"])
                    .build();
                let w = self.clone();
                delete.connect_clicked(move |_| {
                    w.0.ctl.edit(|c| {
                        c.profiles.remove(i);
                        if c.active_profile >= i && c.active_profile > 0 {
                            c.active_profile -= 1;
                        }
                    });
                    w.reload();
                });
                row.add_suffix(&delete);
            }
            profiles.add(&row);
        }
        let add = adw::EntryRow::builder().title("New profile from the current one").show_apply_button(true).build();
        let w = self.clone();
        add.connect_apply(move |row| {
            let name = row.text().trim().to_string();
            if name.is_empty() {
                return;
            }
            w.0.ctl.edit(|c| {
                let profile = Profile { name, ..c.profile().clone() };
                c.profiles.push(profile);
                c.active_profile = c.profiles.len() - 1;
            });
            w.reload();
        });
        profiles.add(&add);
        page.add(&profiles);
        page
    }

    fn smart_page(&self) -> adw::PreferencesPage {
        let ctl = self.ctl();
        let smart = ctl.cfg.borrow().smart;
        let page = adw::PreferencesPage::new();

        let live = group("Live view", "Type here or let the cat walk: keys light up as Smart Detect sees them.");
        let status = gtk::Box::builder().spacing(8).halign(gtk::Align::Center).margin_bottom(6).build();
        let icon = gtk::Image::from_icon_name("object-select-symbolic");
        let label = gtk::Label::builder().label("All quiet").css_classes(["heading"]).build();
        status.append(&icon);
        status.append(&label);
        *self.0.chaos.borrow_mut() = Some((icon, label));
        let heatmap = HeatMap::new();
        let card =
            gtk::Box::builder().orientation(gtk::Orientation::Vertical).css_classes(["card", "heatmap-card"]).build();
        card.append(&status);
        card.append(&heatmap.widget);
        live.add(&card);
        *self.0.heatmap.borrow_mut() = Some(heatmap);
        page.add(&live);

        let settings =
            group("Smart Detect", "Runs entirely on this computer. Keystrokes are only counted, never stored or sent.");
        let enabled = adw::SwitchRow::builder()
            .title("Lock automatically")
            .subtitle("Lock the moment the keyboard looks like a cat is on it")
            .active(smart.enabled)
            .build();
        let c = ctl.clone();
        enabled.connect_active_notify(move |r| c.edit(|cfg| cfg.smart.enabled = r.is_active()));
        settings.add(&enabled);

        let sensitivity = adw::ActionRow::builder()
            .title("Sensitivity")
            .subtitle("Higher catches gentler paws; very fast typing may set it off")
            .build();
        let toggles = adw::ToggleGroup::builder().valign(gtk::Align::Center).can_shrink(false).build();
        for label in SENSITIVITY_LABELS {
            toggles.add(adw::Toggle::builder().label(label).build());
        }
        toggles.set_active(u32::from(smart.sensitivity.clamp(1, 5) - 1));
        let c = ctl.clone();
        toggles.connect_active_notify(move |t| c.edit(|cfg| cfg.smart.sensitivity = t.active() as u8 + 1));
        sensitivity.add_suffix(&toggles);
        settings.add(&sensitivity);

        let ignore = adw::SwitchRow::builder()
            .title("Ignore single key holds")
            .subtitle("Never lock because one key stays down. Holds only count at High and Highest.")
            .active(smart.ignore_single_holds)
            .build();
        let c = ctl.clone();
        ignore.connect_active_notify(move |r| c.edit(|cfg| cfg.smart.ignore_single_holds = r.is_active()));
        settings.add(&ignore);

        let l = smart.learning;
        let learned = adw::ActionRow::builder()
            .title("Learned from false alarms")
            .subtitle(if l.is_empty() {
                "Tap “That wasn't my cat” on the lock screen and the rule that fired relaxes.".to_string()
            } else {
                format!(
                    "Relaxed by: paw {:+.1}, resting {:+.1}, hold {:+.1}, zoomies {:+.1} levels",
                    -l.paw, -l.resting, -l.hold, -l.zoomies
                )
            })
            .build();
        let reset = gtk::Button::builder().label("Reset").valign(gtk::Align::Center).sensitive(!l.is_empty()).build();
        let w = self.clone();
        reset.connect_clicked(move |_| {
            w.0.ctl.edit(|cfg| cfg.smart.learning = Default::default());
            w.reload();
        });
        learned.add_suffix(&reset);
        settings.add(&learned);
        page.add(&settings);
        page
    }

    fn lock_page(&self) -> adw::PreferencesPage {
        let ctl = self.ctl();
        let cfg = ctl.cfg.borrow();
        let profile = cfg.profile().clone();
        let page = adw::PreferencesPage::new();

        let modes = group("Lock mode", &format!("For the “{}” profile.", profile.name));
        let mut first: Option<gtk::CheckButton> = None;
        for mode in LockMode::ALL {
            let subtitle = match mode {
                LockMode::Full => "Blocks every key, covers the screen and plays your deterrent sound.",
                LockMode::Stealth => "Blocks every key with no overlay and no sound. Good for meetings and movies.",
                LockMode::BuiltIn => {
                    "Blocks only the laptop keyboard so an external one keeps working. Unlocks when the external keyboard is unplugged."
                }
            };
            let row = adw::ActionRow::builder().title(mode.label()).subtitle(subtitle).build();
            let check = gtk::CheckButton::builder().active(profile.mode == mode).valign(gtk::Align::Center).build();
            check.set_group(first.as_ref());
            first.get_or_insert(check.clone());
            let w = self.clone();
            check.connect_toggled(move |b| {
                if b.is_active() {
                    w.edit_profile(|p| p.mode = mode);
                }
            });
            row.add_prefix(&check);
            row.set_activatable_widget(Some(&check));
            modes.add(&row);
        }
        page.add(&modes);

        let extras = group("While locked", "");
        let awake = adw::SwitchRow::builder()
            .title("Keep the computer awake")
            .subtitle("No screen blanking, session lock or suspend while locked: long renders, builds and backups keep running")
            .active(profile.keep_awake)
            .build();
        let w = self.clone();
        awake.connect_active_notify(move |r| w.edit_profile(|p| p.keep_awake = r.is_active()));
        extras.add(&awake);
        let touchpad = adw::SwitchRow::builder()
            .title("Lock the touchpad too")
            .subtitle("The laptop touchpad and pointing stick stop responding; external mice keep working")
            .active(profile.lock_touchpad)
            .build();
        let w = self.clone();
        touchpad.connect_active_notify(move |r| w.edit_profile(|p| p.lock_touchpad = r.is_active()));
        extras.add(&touchpad);
        page.add(&extras);

        let shortcut =
            group("Shortcut", "Works in every app. It needs Ctrl, Alt or Super, so a paw cannot press it by accident.");
        let row = adw::ActionRow::builder().title("Lock and unlock").build();
        let label = adw::ShortcutLabel::new(&cfg.shortcut.accelerator());
        label.set_valign(gtk::Align::Center);
        let recording = self.0.recording.get();
        let change = gtk::Button::builder()
            .label(if recording { "Press the new shortcut…" } else { "Change…" })
            .valign(gtk::Align::Center)
            .build();
        if recording {
            change.add_css_class("suggested-action");
        }
        let w = self.clone();
        change.connect_clicked(move |_| {
            let on = !w.0.recording.get();
            w.0.recording.set(on);
            w.0.ctl.send(Command::RecordShortcut(on));
            w.reload();
        });
        row.add_suffix(&label);
        row.add_suffix(&change);
        shortcut.add(&row);
        page.add(&shortcut);
        page
    }

    fn screen_page(&self) -> adw::PreferencesPage {
        let ctl = self.ctl();
        let screen = ctl.cfg.borrow().profile().screen.clone();
        let page = adw::PreferencesPage::new();

        let backgrounds = group("Background", "");
        let tiles = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .max_children_per_line(7)
            .column_spacing(12)
            .row_spacing(12)
            .build();
        for (id, name) in BACKGROUNDS {
            let swatch = gtk::Box::builder().css_classes(["bg-tile", &format!("bg-{id}")]).build();
            if screen.background == *id {
                swatch.add_css_class("selected");
            }
            let w = self.clone();
            let id = id.to_string();
            tiles.append(&tile(&swatch, name, move || {
                w.edit_profile(|p| p.screen.background = id.clone());
                w.reload();
            }));
        }
        let photo_swatch = gtk::Box::builder().css_classes(["bg-tile", "photo-tile"]).build();
        match screen.photo.as_ref().filter(|p| p.exists()) {
            Some(path) => {
                let picture = gtk::Picture::builder().content_fit(gtk::ContentFit::Cover).hexpand(true).build();
                picture.set_filename(Some(path));
                photo_swatch.append(&picture);
            }
            None => {
                photo_swatch.append(&gtk::Image::builder().icon_name("image-x-generic-symbolic").hexpand(true).build())
            }
        }
        if screen.background == "photo" {
            photo_swatch.add_css_class("selected");
        }
        let w = self.clone();
        tiles.append(&tile(&photo_swatch, "Photo…", move || w.pick_photo()));
        backgrounds.add(&tiles);
        page.add(&backgrounds);

        let icons = group("Icon", "");
        let icon_tiles = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .max_children_per_line(10)
            .column_spacing(12)
            .row_spacing(12)
            .build();
        let custom = !screen.icon.is_empty() && !ICONS.iter().any(|(e, _)| *e == screen.icon);
        let mut choices: Vec<(String, String)> = ICONS.iter().map(|(e, n)| (e.to_string(), n.to_string())).collect();
        if custom {
            choices.push((screen.icon.clone(), "Custom".into()));
        }
        choices.push((String::new(), "None".into()));
        for (emoji, name) in choices {
            let face = gtk::Label::builder()
                .label(if emoji.is_empty() { "∅" } else { &emoji })
                .css_classes(["icon-tile"])
                .build();
            if emoji == screen.icon {
                face.add_css_class("selected");
            }
            let w = self.clone();
            icon_tiles.append(&tile(&face, &name, move || {
                w.edit_profile(|p| p.screen.icon = emoji.clone());
                w.reload();
            }));
        }
        let chooser = gtk::EmojiChooser::new();
        let w = self.clone();
        chooser.connect_emoji_picked(move |_, emoji| {
            let emoji = emoji.to_string();
            w.edit_profile(|p| p.screen.icon = emoji);
            w.reload();
        });
        let more = gtk::MenuButton::builder()
            .popover(&chooser)
            .child(&gtk::Label::builder().label("☺").css_classes(["icon-tile"]).build())
            .css_classes(["flat", "tile-button"])
            .tooltip_text("Any emoji")
            .build();
        let more_box = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).build();
        more_box.append(&more);
        more_box.append(&gtk::Label::builder().label("Emoji…").css_classes(["caption"]).build());
        icon_tiles.append(&more_box);
        icons.add(&icon_tiles);
        page.add(&icons);

        let text = group("Message", "");
        let message = adw::EntryRow::builder()
            .title(format!("Display message (default: {DEFAULT_MESSAGE})"))
            .text(&screen.message)
            .build();
        let w = self.clone();
        message.connect_changed(move |row| {
            let text = row.text().to_string();
            w.edit_profile(|p| p.screen.message = text);
        });
        text.add(&message);
        let flash = adw::SwitchRow::builder()
            .title("Flash on blocked keys")
            .subtitle("The screen edge glows each time a key is blocked")
            .active(screen.flash)
            .build();
        let w = self.clone();
        flash.connect_active_notify(move |r| w.edit_profile(|p| p.screen.flash = r.is_active()));
        text.add(&flash);
        let preview =
            adw::ButtonRow::builder().title("Preview Lock Screen").start_icon_name("video-display-symbolic").build();
        let c = ctl.clone();
        preview.connect_activated(move |_| c.preview_lock_screen());
        text.add(&preview);
        page.add(&text);
        page
    }

    fn pick_photo(&self) {
        let filter = gtk::FileFilter::new();
        filter.add_mime_type("image/*");
        filter.set_name(Some("Images"));
        let dialog =
            gtk::FileDialog::builder().title("Choose a Lock Screen Photo").modal(true).default_filter(&filter).build();
        let w = self.clone();
        dialog.open(Some(&self.0.window), gio::Cancellable::NONE, move |result| {
            if let Some(path) = result.ok().and_then(|f| f.path()) {
                w.edit_profile(|p| {
                    p.screen.photo = Some(path);
                    p.screen.background = "photo".into();
                });
                w.reload();
            }
        });
    }

    fn sound_page(&self) -> adw::PreferencesPage {
        let ctl = self.ctl();
        let profile = ctl.cfg.borrow().profile().clone();
        let page = adw::PreferencesPage::new();
        let sounds = group(
            "Deterrent sound",
            "Plays each time a key is pressed while locked (not in Stealth or Built-in locks). Import your own short clip, up to 15 seconds and 5 MB.",
        );
        let mut entries: Vec<(String, String, &str, bool)> =
            sound::BUILTIN.iter().map(|b| (b.id.to_string(), b.label.to_string(), b.icon, false)).collect();
        entries.extend(
            sound::custom_sounds().into_iter().map(|n| (sound::custom_id(&n), n, "folder-music-symbolic", true)),
        );
        entries.push((sound::SILENT.into(), "Silent".into(), "audio-volume-muted-symbolic", false));

        let mut first: Option<gtk::CheckButton> = None;
        for (id, label, icon, custom) in entries {
            let row = adw::ActionRow::builder().title(&label).build();
            let check = gtk::CheckButton::builder().active(profile.sound == id).valign(gtk::Align::Center).build();
            check.set_group(first.as_ref());
            first.get_or_insert(check.clone());
            row.add_prefix(&gtk::Image::from_icon_name(icon));
            row.add_prefix(&check);
            row.set_activatable_widget(Some(&check));
            let w = self.clone();
            let chosen = id.clone();
            check.connect_toggled(move |b| {
                if b.is_active() {
                    w.edit_profile(|p| p.sound = chosen.clone());
                }
            });
            if id != sound::SILENT {
                let play = gtk::Button::builder()
                    .icon_name("media-playback-start-symbolic")
                    .valign(gtk::Align::Center)
                    .tooltip_text("Preview")
                    .css_classes(["flat", "circular"])
                    .build();
                let c = ctl.clone();
                let preview = id.clone();
                play.connect_clicked(move |_| {
                    let volume = c.cfg.borrow().profile().volume;
                    c.player.preview(&preview, volume);
                });
                row.add_suffix(&play);
            }
            if custom {
                let delete = gtk::Button::builder()
                    .icon_name("user-trash-symbolic")
                    .valign(gtk::Align::Center)
                    .tooltip_text("Remove clip")
                    .css_classes(["flat"])
                    .build();
                let w = self.clone();
                let removed = id.clone();
                delete.connect_clicked(move |_| {
                    sound::remove_custom(&removed);
                    w.0.ctl.edit(|c| {
                        for p in c.profiles.iter_mut().filter(|p| p.sound == removed) {
                            p.sound = "warning".into();
                        }
                    });
                    w.reload();
                });
                row.add_suffix(&delete);
            }
            sounds.add(&row);
        }
        let import = adw::ButtonRow::builder().title("Import Sound…").start_icon_name("list-add-symbolic").build();
        let w = self.clone();
        import.connect_activated(move |_| w.import_sound());
        sounds.add(&import);
        page.add(&sounds);

        let volume_group = group("Volume", "");
        let volume = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.05);
        volume.set_value(f64::from(profile.volume));
        volume.set_hexpand(true);
        volume.set_valign(gtk::Align::Center);
        let w = self.clone();
        volume.connect_value_changed(move |s| w.edit_profile(|p| p.volume = s.value() as f32));
        let row = adw::ActionRow::builder().title("Deterrent volume").build();
        row.add_suffix(&volume);
        volume_group.add(&row);
        page.add(&volume_group);
        page
    }

    fn import_sound(&self) {
        let filter = gtk::FileFilter::new();
        filter.add_mime_type("audio/*");
        filter.set_name(Some("Audio"));
        let dialog = gtk::FileDialog::builder().title("Import a Sound").modal(true).default_filter(&filter).build();
        let w = self.clone();
        dialog.open(Some(&self.0.window), gio::Cancellable::NONE, move |result| {
            let Some(path) = result.ok().and_then(|f| f.path()) else { return };
            match sound::import(&path) {
                Ok(id) => {
                    w.edit_profile(|p| p.sound = id);
                    w.reload();
                }
                Err(e) => w.toast(&e),
            }
        });
    }

    fn general_page(&self) -> adw::PreferencesPage {
        let ctl = self.ctl();
        let page = adw::PreferencesPage::new();

        let startup = group("Startup", "");
        let login = adw::SwitchRow::builder()
            .title("Launch at login")
            .subtitle("Start in the tray so Smart Detect is always on guard")
            .active(system::launch_at_login())
            .build();
        let w = self.clone();
        login.connect_active_notify(move |r| {
            if let Err(e) = system::set_launch_at_login(r.is_active()) {
                w.toast(&format!("Cannot change autostart: {e}"));
            }
        });
        startup.add(&login);
        page.add(&startup);

        let devices = ctl.devices.borrow();
        let found = group(
            "Devices",
            "Every keyboard is guarded. Built-in devices are the ones the Built-in keyboard mode and touchpad lock act on.",
        );
        for kb in &devices.keyboards {
            let row = adw::ActionRow::builder()
                .title(&kb.name)
                .subtitle(if kb.built_in { "Built-in keyboard" } else { "External keyboard" })
                .build();
            row.add_prefix(&gtk::Image::from_icon_name("input-keyboard-symbolic"));
            found.add(&row);
        }
        for name in &devices.touchpads {
            let row = adw::ActionRow::builder().title(name).subtitle("Built-in pointer").build();
            row.add_prefix(&gtk::Image::from_icon_name("input-touchpad-symbolic"));
            found.add(&row);
        }
        if devices.keyboards.is_empty() || devices.denied > 0 {
            found.add(&permission_help());
        }
        page.add(&found);

        let about = group("About", "");
        let privacy = adw::ActionRow::builder()
            .title("Privacy")
            .subtitle("No accounts, no cloud, no analytics. Catlock never records what you type.")
            .build();
        about.add(&privacy);
        let about_row = adw::ButtonRow::builder().title("About Catlock").build();
        let w = self.clone();
        about_row.connect_activated(move |_| {
            adw::AboutDialog::builder()
                .application_name("Catlock")
                .application_icon(APP_ID)
                .version(env!("CARGO_PKG_VERSION"))
                .comments("Keyboard lock for cat people. Lock the keys, love the cat.")
                .developer_name("Andrea Niccoli")
                .build()
                .present(Some(&w.0.window));
        });
        about.add(&about_row);
        page.add(&about);
        page
    }
}

/// A clickable swatch with a caption underneath.
fn tile(face: &impl IsA<gtk::Widget>, caption: &str, on_click: impl Fn() + 'static) -> gtk::Box {
    let button = gtk::Button::builder().child(face).css_classes(["flat", "tile-button"]).tooltip_text(caption).build();
    button.connect_clicked(move |_| on_click());
    let column = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).build();
    column.append(&button);
    column.append(&gtk::Label::builder().label(caption).css_classes(["caption"]).build());
    column
}

fn permission_help() -> adw::ActionRow {
    adw::ActionRow::builder()
        .title("Some input devices are not accessible")
        .subtitle(
            "Catlock reads keyboards through /dev/input. Run “sudo usermod -aG input $USER”, then log out and back in.",
        )
        .subtitle_selectable(true)
        .css_classes(["warning"])
        .build()
}

#[cfg(debug_assertions)]
fn save_png(window: &gtk::Window, path: &std::path::Path) {
    let paintable = gtk::WidgetPaintable::new(Some(window));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, f64::from(window.width()), f64::from(window.height()));
    let (Some(node), Some(renderer)) = (snapshot.to_node(), window.renderer()) else { return };
    let texture = renderer.render_texture(node, None);
    if let Err(e) = texture.save_to_png(path) {
        eprintln!("catlock: snapshot {}: {e}", path.display());
    }
}
