#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use gpui::{App, AppContext, Application, Bounds, WindowBounds, WindowOptions, point, px, size};
use surf_ui::SurfApp;

fn main() {
    Application::new().run(|cx: &mut App| {
        // A fixed top-left origin behaves predictably on high-DPI Windows
        // displays; the window is deliberately smaller than a 1440p desktop.
        let bounds = Bounds {
            origin: point(px(40.0), px(40.0)),
            size: size(px(1150.0), px(680.0)),
        };
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| SurfApp::new(window, cx)),
        )
        .expect("failed to open the Surf window");
        cx.activate(true);
    });
}
