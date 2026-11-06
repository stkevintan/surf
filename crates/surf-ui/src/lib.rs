//! Surf's GPUI interface: the Logic-style window from `docs/design/`.
//!
//! - [`session`] is everything behind the interface (engine, MIDI, plugins).
//! - [`shell`] is the window and its UI state; [`views`] draws each area.
//! - [`theme`] holds the design tokens; [`widgets`] the shared controls.

pub mod dialogs;
pub mod editing;
pub mod fonts;
pub mod icons;
pub mod session;
pub mod shell;
pub mod theme;
pub mod views;
pub mod widgets;

pub use icons::Assets;
pub use shell::SurfApp;

#[cfg(test)]
mod editor_tests;
