#![warn(clippy::all, rust_2018_idioms)]

mod app;
mod events;
mod gitinfo;
mod shell;
mod tabs;

fn main() -> eframe::Result {
    env_logger::init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 600.0])
            .with_min_inner_size([320.0, 240.0]),
        ..Default::default()
    };

    eframe::run_native(
        "repterm",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)?))),
    )
}
