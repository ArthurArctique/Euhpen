//! Installateur pour Windows et macOS : une fenêtre qui installe Euhpen et tout ce dont il a besoin.
//!
//! Tout va dans un seul dossier de l'utilisateur, sans droits d'administrateur :
//!
//! ```text
//! l'application, la ligne de commande, de quoi réparer ou désinstaller, python/
//! .venv/              environnement Python du modèle
//! tools/              ffmpeg, ffprobe, uv et Python 3.12
//! ```
//!
//! `install` fait le travail, `job` suit son avancement, `wizard` dessine les pages et
//! `platform` (`windows.rs` ou `macos.rs`) réunit ce qui change d'un système à l'autre.

mod install;
mod job;
#[cfg_attr(windows, path = "windows.rs")]
#[cfg_attr(target_os = "macos", path = "macos.rs")]
mod platform;
mod wizard;

pub fn run() -> Result<(), String> {
    let uninstalling = platform::launched_as_uninstaller() || std::env::args().any(|argument| argument == "--uninstall");
    wizard::run(uninstalling).inspect_err(|message| {
        // Sans console, une erreur à l'ouverture de la fenêtre passerait inaperçue.
        rfd::MessageDialog::new().set_title("Euhpen").set_description(message).set_level(rfd::MessageLevel::Error).show();
    })
}
