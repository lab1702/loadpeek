mod app;
mod chart;
mod history;
mod metrics;
mod process_view;
mod processes;
mod settings;
mod theme;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Loadpeek · System monitor")
            .with_inner_size([1440.0, 1000.0])
            .with_min_inner_size([640.0, 480.0]),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "Loadpeek",
        options,
        Box::new(|cc| Ok(Box::new(app::Loadpeek::new(cc)))),
    )
}
