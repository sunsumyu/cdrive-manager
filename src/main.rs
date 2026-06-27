mod app;
mod cleanup;
mod format;
mod model;
mod scanner;
mod treemap;

use app::CDriveManagerApp;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("C 盘空间管理器")
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([900.0, 620.0]),
        ..Default::default()
    };

    eframe::run_native(
        "C 盘空间管理器",
        options,
        Box::new(|cc| Ok(Box::new(CDriveManagerApp::new(cc)))),
    )
}
