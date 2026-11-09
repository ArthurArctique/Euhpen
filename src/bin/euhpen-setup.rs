//! Installateur d'Euhpen pour Windows et macOS. `euhpen-setup` installe, `euhpen-setup --uninstall` désinstalle.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(all(any(windows, target_os = "macos"), feature = "installer"))]
fn main() {
    if let Err(message) = euhpen::setup::run() {
        eprintln!("Erreur : {message}");
        std::process::exit(1);
    }
}

#[cfg(not(all(any(windows, target_os = "macos"), feature = "installer")))]
fn main() {
    eprintln!("L'installateur n'existe que pour Windows et macOS.");
    std::process::exit(1);
}
