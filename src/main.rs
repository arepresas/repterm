#![warn(clippy::all, rust_2018_idioms)]

mod app;
mod chat_view;
mod events;
mod gitinfo;
mod llm;
mod settings;
mod shell;
mod tabs;
mod tools;

fn main() -> eframe::Result {
    env_logger::init();

    // GUI launchers such as Nautilus provide no TERM. Shells need one to emit
    // OSC titles, which repterm uses to track the active directory.
    alacritty_terminal::tty::setup_env();

    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 600.0])
            .with_min_inner_size([320.0, 240.0]),
        ..Default::default()
    };
    // Release builds pick Vulkan first. On AMD Vega that corrupts text and
    // drops repaints. GL matches `WGPU_BACKEND=gl`, which renders correctly.
    #[cfg(target_os = "linux")]
    prefer_gl_unless_overridden(&mut options);

    eframe::run_native(
        "repterm",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)?))),
    )
}

/// Use GL on Linux unless `WGPU_BACKEND` already selects a backend.
#[cfg(target_os = "linux")]
fn prefer_gl_unless_overridden(options: &mut eframe::NativeOptions) {
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut options.wgpu_options.wgpu_setup {
        setup.instance_descriptor.backends =
            eframe::wgpu::Backends::from_env().unwrap_or(eframe::wgpu::Backends::GL);
    }
}
