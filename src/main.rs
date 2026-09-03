// Desktop entry point. On Android the crate is built as a cdylib and started
// through `android_main` in lib.rs instead, so this binary is empty there.

#[cfg(not(target_os = "android"))]
fn main() -> eframe::Result<()> {
    use drone_app::app::DroneApp;
    use drone_app::config::{AppConfig, CONFIG_FILE};

    env_logger::init();

    // Beside the working directory, which for a ground station is the checkout
    // the app was started from. Android puts it in the app's data dir instead
    // (see lib.rs), that being the only writable place there.
    let mut config = AppConfig::load(CONFIG_FILE);
    // A key file named without a directory sits beside the config, so a
    // checkout can carry both and be moved as one.
    config.resolve_key_path(".");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([960.0, 640.0])
            .with_title("drone-app"),
        ..Default::default()
    };

    eframe::run_native(
        "drone-app",
        options,
        Box::new(|cc| {
            // No safe-area insets on desktop: nothing covers the window.
            Ok(Box::new(DroneApp::new(cc.egui_ctx.clone(), config, None)))
        }),
    )
}

#[cfg(target_os = "android")]
fn main() {}
