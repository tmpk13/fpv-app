pub mod app;
pub mod config;
pub mod video;

/// Android entry point.
///
/// The Android activity glue calls the exported `android_main` symbol. Desktop
/// uses `src/main.rs` instead; both build the same `app::DroneApp`.
#[cfg(target_os = "android")]
#[no_mangle]
fn android_main(android_app: egui_winit::winit::platform::android::activity::AndroidApp) {
    use eframe::{NativeOptions, Renderer};

    android_logger::init_once(
        android_logger::Config::default()
            .with_tag("drone-app")
            .with_max_level(log::LevelFilter::Info),
    );

    // The working directory is not writable on Android, so the config lives in
    // the app's private data dir. It is not reachable from the phone's file
    // manager, which is why the Settings page exists rather than expecting the
    // file to be edited by hand as it can be on desktop.
    let config_path = android_app
        .internal_data_path()
        .map(|dir| dir.join(config::CONFIG_FILE));
    let config = match config_path {
        Some(path) => config::AppConfig::load(path),
        None => config::AppConfig::default(),
    };

    // Safe-area insets: `content_rect` is the region inside the system bars.
    // Updates on rotation via the InsetsChanged event, so query it each frame
    // rather than once here. The video deliberately paints under all of it;
    // only the controls are kept clear.
    let insets_app = android_app.clone();
    let insets: Option<Box<dyn Fn() -> [f32; 4]>> = Some(Box::new(move || {
        let rect = insets_app.content_rect();
        let (w, h) = insets_app
            .native_window()
            .map(|win| (win.width(), win.height()))
            .unwrap_or((0, 0));
        let top = rect.top.max(0) as f32;
        let left = rect.left.max(0) as f32;
        let right = if w > 0 {
            (w - rect.right).max(0) as f32
        } else {
            0.0
        };
        let bottom = if h > 0 {
            (h - rect.bottom).max(0) as f32
        } else {
            0.0
        };
        [top, right, bottom, left]
    }));

    let mut options = NativeOptions::default();
    options.renderer = Renderer::Wgpu;
    options.android_app = Some(android_app);

    let _ = eframe::run_native(
        "drone-app",
        options,
        Box::new(move |cc| {
            Ok(Box::new(app::DroneApp::new(
                cc.egui_ctx.clone(),
                config,
                insets,
            )))
        }),
    );
}
