#![windows_subsystem = "windows"]

fn main() {
    let fresh = std::env::args().any(|a| a == "--fresh");
    if let Err(message) = euhpen::gui::run(fresh) {
        eprintln!("Erreur : {message}");
        std::process::exit(1);
    }
}
