mod app;
mod cleanup;
mod duplicates;
mod format;
#[cfg(windows)]
mod mft;
mod model;
mod scan_cache;
mod scanner;
mod sunburst;
mod treemap;

use app::CDriveManagerApp;

fn main() -> eframe::Result<()> {
    // Setup Chinese font support
    let mut fonts = egui::FontDefinitions::default();
    
    // Add Chinese font family
    fonts.font_data.insert(
        "chinese".to_owned(),
        egui::FontData::from_static(include_bytes!(
            "C:\\Windows\\Fonts\\msyh.ttc"
        ))
        .into(),
    );
    
    // Set Chinese font as primary for prose text
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "chinese".to_owned());
    
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .insert(0, "chinese".to_owned());

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
        Box::new(|cc| {
            cc.egui_ctx.set_fonts(fonts);
            Ok(Box::new(CDriveManagerApp::new(cc)))
        }),
    )
}
