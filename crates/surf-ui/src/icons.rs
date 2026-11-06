//! Toolbar and transport icons, bundled as SVG.
//!
//! GPUI draws SVGs as a monochrome mask tinted with the element's text color,
//! so every icon is plain strokes on a 16x16 grid, the same shapes as the
//! design prototype.

use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

/// Serves the icons under `icons/<name>.svg`.
pub struct Assets;

const ICONS: &[(&str, &str)] = &[
    (
        "library",
        r#"<rect x="1.5" y="2.5" width="13" height="11" rx="1.5"/><path d="M6 2.5v11M2.8 5.5h2M2.8 7.5h2M2.8 9.5h2"/>"#,
    ),
    (
        "inspector",
        r#"<circle cx="8" cy="8" r="6.3"/><path d="M8 7v4.2"/><circle cx="8" cy="4.9" r=".6" fill="black"/>"#,
    ),
    (
        "toolbar",
        r#"<rect x="1.5" y="2.5" width="13" height="11" rx="1.5"/><path d="M1.5 6h13"/>"#,
    ),
    (
        "smart",
        r#"<circle cx="5" cy="8" r="3"/><circle cx="11.5" cy="8" r="3"/><path d="M5 8l1.6-1.6M11.5 8l-1.2-1.8"/>"#,
    ),
    (
        "mixer",
        r#"<path d="M4 2v12M8 2v12M12 2v12"/><rect x="2.5" y="8.5" width="3" height="2" rx=".5" fill="black"/><rect x="6.5" y="4.5" width="3" height="2" rx=".5" fill="black"/><rect x="10.5" y="10" width="3" height="2" rx=".5" fill="black"/>"#,
    ),
    (
        "editors",
        r#"<path d="M10.8 2.2l3 3-8 8H2.8v-3zM9.2 3.8l3 3"/>"#,
    ),
    (
        "rewind",
        r#"<path d="M8 3.5v9L2 8zM14.5 3.5v9l-6-4.5z" fill="black" stroke="none"/>"#,
    ),
    (
        "forward",
        r#"<path d="M8 3.5v9L14 8zM1.5 3.5v9l6-4.5z" fill="black" stroke="none"/>"#,
    ),
    (
        "stop",
        r#"<rect x="3.5" y="3.5" width="9" height="9" rx="1" fill="black" stroke="none"/>"#,
    ),
    (
        "play",
        r#"<path d="M4.5 2.8v10.4L13 8z" fill="black" stroke="none"/>"#,
    ),
    (
        "pause",
        r#"<rect x="3.5" y="2.8" width="3" height="10.4" rx=".5" fill="black" stroke="none"/><rect x="9.5" y="2.8" width="3" height="10.4" rx=".5" fill="black" stroke="none"/>"#,
    ),
    (
        "record",
        r#"<circle cx="8" cy="8" r="4.6" fill="black" stroke="none"/>"#,
    ),
    (
        "cycle",
        r#"<path d="M2.5 9V7.5a3 3 0 0 1 3-3h8M11.5 2.5l2 2-2 2M13.5 7v1.5a3 3 0 0 1-3 3h-8M4.5 13.5l-2-2 2-2"/>"#,
    ),
    (
        "metronome",
        r#"<path d="M5.6 1.8h4.8l2.6 12.4H3zM8 11l3.6-6.4"/>"#,
    ),
    (
        "typing",
        r#"<rect x="1.5" y="4" width="13" height="8.5" rx="1.3"/><path d="M4 6.5h1M7.5 6.5h1M11 6.5h1M4 9.5h8"/>"#,
    ),
    (
        "list",
        r#"<path d="M5.5 4h8M5.5 8h8M5.5 12h8"/><circle cx="2.8" cy="4" r=".8" fill="black"/><circle cx="2.8" cy="8" r=".8" fill="black"/><circle cx="2.8" cy="12" r=".8" fill="black"/>"#,
    ),
    (
        "notepad",
        r#"<rect x="3" y="1.8" width="10" height="12.4" rx="1.3"/><path d="M5.5 5.5h5M5.5 8h5M5.5 10.5h3"/>"#,
    ),
    (
        "loops",
        r#"<circle cx="8" cy="8" r="5.8"/><circle cx="8" cy="8" r="2"/>"#,
    ),
    ("plus", r#"<path d="M8 3v10M3 8h10"/>"#),
    ("close", r#"<path d="M4 4l8 8M12 4l-8 8"/>"#),
    (
        "maximize",
        r#"<path d="M3 10V13h3M13 6V3h-3M3 13l4-4M13 3l-4 4"/>"#,
    ),
    ("chevron", r#"<path d="M4 6l4 4 4-4"/>"#),
    (
        "search",
        r#"<circle cx="7" cy="7" r="5"/><path d="M11 11l3.5 3.5"/>"#,
    ),
    (
        "piano",
        r#"<rect x="1.5" y="3" width="13" height="10" rx="1"/><path d="M4.8 3v6M8 3v6M11.2 3v6" stroke-width="1.8"/>"#,
    ),
    (
        "keys",
        r#"<rect x="1.5" y="5" width="13" height="7" rx="1"/><path d="M4.5 5v4M8 5v4M11.5 5v4"/>"#,
    ),
];

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        let Some(name) = path
            .strip_prefix("icons/")
            .and_then(|p| p.strip_suffix(".svg"))
        else {
            return Ok(None);
        };
        Ok(ICONS.iter().find(|(n, _)| *n == name).map(|(_, body)| {
            Cow::Owned(
                format!(
                    r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="black" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round">{body}</svg>"#
                )
                .into_bytes(),
            )
        }))
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        if path.trim_end_matches('/') != "icons" {
            return Ok(Vec::new());
        }
        Ok(ICONS
            .iter()
            .map(|(n, _)| format!("icons/{n}.svg").into())
            .collect())
    }
}

/// Asset path for a named icon.
pub fn path(name: &str) -> SharedString {
    format!("icons/{name}.svg").into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_is_well_formed_svg() {
        for (name, _) in ICONS {
            let bytes = Assets.load(&format!("icons/{name}.svg")).unwrap().unwrap();
            let text = std::str::from_utf8(&bytes).unwrap();
            assert!(
                text.starts_with("<svg") && text.ends_with("</svg>"),
                "{name}"
            );
            assert_eq!(
                text.matches('<').count(),
                text.matches('>').count(),
                "{name}"
            );
        }
        assert!(Assets.load("icons/nope.svg").unwrap().is_none());
    }
}
