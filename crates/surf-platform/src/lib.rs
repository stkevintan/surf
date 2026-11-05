//! The OS seams Surf needs, behind one interface per concern.
//!
//! Everything above this crate is platform-independent. Each module has one
//! implementation per OS, selected with `cfg`.

pub mod dirs;
pub mod rt;
pub mod shm;

pub use dirs::data_dir;
