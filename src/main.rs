mod config;
mod detect;
mod engine;
mod keys;
mod sound;
mod system;
mod tray;
mod ui;
mod uinput;

use std::cell::OnceCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use config::APP_ID;
use ui::Controller;

/// Whether a Catlock instance already owns its D-Bus name.
fn already_running() -> bool {
    let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else { return false };
    bus.call_sync(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "NameHasOwner",
        Some(&(APP_ID,).to_variant()),
        Some(glib::VariantTy::new("(b)").expect("valid type")),
        gio::DBusCallFlags::NONE,
        1000,
        gio::Cancellable::NONE,
    )
    .ok()
    .and_then(|reply| reply.get::<(bool,)>())
    .is_some_and(|(owned,)| owned)
}

fn main() -> glib::ExitCode {
    // Queries and stop requests must not start an instance (and grab every
    // keyboard) just to answer them.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{}", ui::HELP);
        return glib::ExitCode::SUCCESS;
    }
    let query_only = ["--status", "--unlock", "--quit"];
    if !args.is_empty() && args.iter().all(|a| query_only.contains(&a.as_str())) && !already_running() {
        if args.iter().any(|a| a == "--status") {
            println!("not running");
        }
        return glib::ExitCode::SUCCESS;
    }

    // The engine and tray run on tokio; GTK keeps the main thread.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("catlock-io")
        .enable_all()
        .build()
        .expect("tokio runtime");

    let app =
        adw::Application::builder().application_id(APP_ID).flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE).build();

    let controller: Rc<OnceCell<Rc<Controller>>> = Rc::default();
    let handle = rt.handle().clone();
    let ctl = controller.clone();
    app.connect_startup(move |app| {
        let display = gdk::Display::default().expect("a display");
        let css = gtk::CssProvider::new();
        css.load_from_string(include_str!("style.css"));
        gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        if let Some(dir) = ui::dev_icon_dir() {
            gtk::IconTheme::for_display(&display).add_search_path(dir);
        }
        gtk::Window::set_default_icon_name(APP_ID);
        let _ = ctl.set(Controller::new(app, handle.clone()));
    });

    // The primary instance gets every command line, including the ones from
    // later `catlock --lock` invocations, which exit as soon as it answers.
    let first = std::cell::Cell::new(true);
    app.connect_command_line(move |_, cmd| {
        let code = controller.get().map_or(1, |c| c.command_line(cmd, first.replace(false)));
        glib::ExitCode::from(code as u8)
    });

    let code = app.run();
    drop(rt);
    code
}
