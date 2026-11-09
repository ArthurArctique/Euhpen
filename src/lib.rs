//! Euhpen : coupe les silences et les hésitations dans des rushes, et exporte une timeline XML
//! pour Premiere Pro et DaVinci Resolve. Seul le modèle (Whisper + tête de détection) tourne
//! dans un processus Python séparé ; tout le reste est ici.

pub mod analysis;
pub mod audio;
pub mod cache;
pub mod cfr;
pub mod cli;
pub mod config;
pub mod corrections;
pub mod decision;
pub mod fcp7;
pub mod files;
pub mod fps;
pub mod gui;
pub mod intervals;
pub mod presets;
pub mod probe;
pub mod proxy;
pub mod settings;
#[cfg(all(any(windows, target_os = "macos"), feature = "installer"))]
pub mod setup;
pub mod timeline;
pub mod transcript;
pub mod worker;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
