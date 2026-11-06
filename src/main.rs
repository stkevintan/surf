#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use gpui::{App, AppContext, Application, Bounds, WindowBounds, WindowOptions, point, px, size};
use surf::ui::{Assets, SurfApp, fonts};

fn main() {
    Application::new().with_assets(Assets).run(|cx: &mut App| {
        fonts::register(cx);
        // A fixed top-left origin behaves predictably on high-DPI Windows
        // displays. 1440x900 fits the full layout (inspector, tracks, editor).
        let bounds = Bounds {
            origin: point(px(40.0), px(40.0)),
            size: size(px(1440.0), px(900.0)),
        };
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(980.0), px(620.0))),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| SurfApp::new(window, cx)),
        )
        .expect("failed to open the Surf window");
        // `SURF_UI` is for scripted screenshots: open in the background so the
        // window doesn't take keyboard focus from whatever the user is doing.
        if std::env::var_os("SURF_UI").is_none() {
            cx.activate(true);
        }
    });
}
