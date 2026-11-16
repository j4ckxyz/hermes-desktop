//! Hermes Desktop: a fast native client for Hermes Agent, built on GPUI.

mod app;
mod assets;
mod guard;
mod notify;
mod onboard;
mod overlays;
#[cfg(test)]
mod tests;
mod theme;
mod ui;
mod view;

use std::sync::Arc;

use gpui::*;
use gpui_component::Root;
use hermes_core::conn::{data_dir, Config};
use hermes_core::store::Store;
use hermes_core::Service;

/// Log to `<data dir>/logs/hermes-desktop.log` (Settings → Advanced). Our crates log at any
/// level; the effective level is `log::set_max_level`, which the Settings window changes live.
fn init_logging(config: &Config) {
    let dir = data_dir().join("logs");
    let _ = std::fs::create_dir_all(&dir);
    let mut b = env_logger::Builder::new();
    b.filter_level(log::LevelFilter::Warn).filter_module("hermes_core", log::LevelFilter::Trace).filter_module("hermes_desktop", log::LevelFilter::Trace).filter_module("hermes_net", log::LevelFilter::Trace);
    if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("hermes-desktop.log")) {
        b.target(env_logger::Target::Pipe(Box::new(f)));
    }
    if std::env::var("RUST_LOG").is_ok() {
        b.parse_default_env();
    }
    let _ = b.try_init();
    log::set_max_level(app::log_level(&config.prefs));
}

fn main() {
    let config = app::bootstrap_config(Config::load());
    init_logging(&config);
    let data = data_dir();
    let store = Arc::new(Store::open(&data.join("cache.sqlite3")).unwrap_or_else(|e| {
        log::warn!("cache unavailable ({e}); running without it");
        Store::memory().expect("in-memory store")
    }));
    let service = Service::start(store, data.join("cache")).expect("start service");

    gpui_platform::application().with_assets(assets::Assets).run(move |cx: &mut App| {
        gpui_component::init(cx);
        cx.on_action(|_: &app::Quit, cx| cx.quit());
        app::bind_keys(cx);
        cx.set_menus(vec![
            Menu { name: "Hermes".into(), items: vec![MenuItem::action("Settings…", app::OpenSettings), MenuItem::separator(), MenuItem::action("Quit Hermes", app::Quit)], disabled: false },
            Menu {
                name: "Bots".into(),
                items: vec![
                    MenuItem::action("Command Palette", app::OpenPalette),
                    MenuItem::action("New Bot", app::NewBot),
                    MenuItem::action("Apps & Connectors", app::OpenApps),
                    MenuItem::action("Bot Info", app::ToggleInfo),
                    MenuItem::action("Pin / Unpin Bot", app::TogglePin),
                    MenuItem::action("Usage", app::OpenUsage),
                    MenuItem::separator(),
                    MenuItem::action("Next Bot", app::NextBot),
                    MenuItem::action("Previous Bot", app::PrevBot),
                ],
                disabled: false,
            },
            Menu { name: "View".into(), items: vec![MenuItem::action("Toggle Sidebar", app::ToggleSidebar), MenuItem::action("Focus Composer", app::FocusComposer)], disabled: false },
            Menu {
                name: "Chat".into(),
                items: vec![MenuItem::action("Attach Files…", app::AttachFiles), MenuItem::action("Copy Conversation", app::CopyConversation), MenuItem::action("Stop Reply", app::StopTurn)],
                disabled: false,
            },
        ]);
        let bounds = Bounds::centered(None, size(px(1180.), px(780.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions { title: Some("Hermes".into()), appears_transparent: true, traffic_light_position: Some(point(px(16.), px(16.))) }),
            // Small enough for a side-by-side half screen; below ~760 px the sidebar steps aside.
            window_min_size: Some(size(px(app::MIN_W), px(app::MIN_H))),
            ..Default::default()
        };
        let service = std::rc::Rc::new(service);
        cx.open_window(options, move |window, cx| {
            let view = cx.new(|cx| app::HermesApp::new(service.clone(), config.clone(), window, cx));
            cx.new(|cx| Root::new(view, window, cx))
        })
        .expect("open window");
        cx.activate(true);
    });
}
