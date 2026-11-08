mod app;
mod btop;
mod config;
mod input;
mod render;
mod ssh;
mod terminal;
mod theme;

fn main() -> eframe::Result {
    if std::env::args().any(|arg| arg == "--version") {
        println!("terminal-omega {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    // Finder starts apps in /, which would be the shells' starting directory.
    if std::env::current_dir().is_ok_and(|dir| dir == std::path::Path::new("/"))
        && let Some(home) = dirs::home_dir()
    {
        let _ = std::env::set_current_dir(home);
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("TerminalOmega")
            .with_app_id("terminal-omega")
            .with_inner_size([1500.0, 900.0])
            .with_min_inner_size([700.0, 420.0]),
        ..Default::default()
    };
    eframe::run_native(
        "TerminalOmega",
        options,
        Box::new(|creation| Ok(Box::new(app::App::new(&creation.egui_ctx)))),
    )
}
