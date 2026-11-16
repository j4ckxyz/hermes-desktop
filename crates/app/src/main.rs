//! Hermes Desktop: a fast native client for Hermes Agent, built on GPUI.

mod app;
mod assets;
mod theme;
mod view;

use std::sync::Arc;

use gpui::*;
use gpui_component::Root;
use hermes_core::conn::{data_dir, Config};
use hermes_core::store::Store;
use hermes_core::Service;

actions!(hermes, [Quit]);

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let data = data_dir();
    let store = Arc::new(Store::open(&data.join("cache.sqlite3")).unwrap_or_else(|e| {
        log::warn!("cache unavailable ({e}); running without it");
        Store::memory().expect("in-memory store")
    }));
    let service = Service::start(store, data.join("cache")).expect("start service");
    let config = app::bootstrap_config(Config::load());

    gpui_platform::application().with_assets(assets::Assets).run(move |cx: &mut App| {
        gpui_component::init(cx);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None), KeyBinding::new("ctrl-q", Quit, None)]);
        cx.set_menus(vec![Menu { name: "Hermes".into(), items: vec![MenuItem::action("Quit Hermes", Quit)], disabled: false }]);
        let bounds = Bounds::centered(None, size(px(1120.), px(760.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions { title: Some("Hermes".into()), appears_transparent: true, traffic_light_position: Some(point(px(14.), px(14.))) }),
            window_min_size: Some(size(px(640.), px(420.))),
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
