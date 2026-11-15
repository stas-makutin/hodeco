//! Native entry point.
//!
//! The same package also produces the WebAssembly library used by the web build
//! script. This binary target is compiled for wasm as an empty stub so
//! `cargo build` can process the package; the browser entry point lives in
//! `lib.rs`.

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use eframe::egui;
    use hodeco_ui::app::App;
    use hodeco_ui::startup::{self, StartupAction};

    pub fn run() -> eframe::Result<()> {
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
        hodeco_ui::runtime::install_adjacent_native();

        let action = match startup::from_native_args(std::env::args().skip(1)) {
            Ok(action) => action,
            Err(error) => {
                eprintln!("error: {error}");
                std::process::exit(2);
            }
        };

        let startup = match action {
            StartupAction::Run(startup) => startup,
            StartupAction::Help => {
                print!("{}", startup::help_text());
                return Ok(());
            }
        };

        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title("hodeco")
                .with_inner_size([1280.0, 820.0])
                .with_min_inner_size([960.0, 600.0]),
            ..Default::default()
        };

        eframe::run_native(
            "hodeco",
            options,
            Box::new(move |cc| Ok(Box::new(App::new(cc, startup)))),
        )
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result<()> {
    native::run()
}

#[cfg(target_arch = "wasm32")]
fn main() {}
