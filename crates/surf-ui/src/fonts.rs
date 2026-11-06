//! Fonts bundled with the app.
//!
//! IBM Plex Sans / Mono and Barlow Condensed are under the SIL Open Font
//! License (see `assets/fonts/OFL-*.txt`), so they ship inside the binary and
//! the interface looks the same on every machine.

use gpui::App;

const FONTS: [&[u8]; 7] = [
    include_bytes!("../assets/fonts/IBMPlexSans-Regular.ttf"),
    include_bytes!("../assets/fonts/IBMPlexSans-Medium.ttf"),
    include_bytes!("../assets/fonts/IBMPlexSans-SemiBold.ttf"),
    include_bytes!("../assets/fonts/IBMPlexMono-Regular.ttf"),
    include_bytes!("../assets/fonts/IBMPlexMono-Medium.ttf"),
    include_bytes!("../assets/fonts/BarlowCondensed-SemiBold.ttf"),
    include_bytes!("../assets/fonts/BarlowCondensed-Bold.ttf"),
];

/// Register the bundled fonts. Call once at startup, before opening a window.
pub fn register(cx: &mut App) {
    let fonts = FONTS
        .iter()
        .map(|bytes| std::borrow::Cow::Borrowed(*bytes))
        .collect();
    if let Err(err) = cx.text_system().add_fonts(fonts) {
        eprintln!("could not load bundled fonts ({err}); falling back to system fonts");
    }
}
