mod app;
mod crypto;
mod model;
mod ops;
mod store;

const KO_FONT_CANDIDATES: &[&str] = &[
    "/usr/share/fonts/truetype/nanum/NanumGothicBold.ttf",
    "/usr/share/fonts/truetype/nanum/NanumGothic.ttf",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
];

fn setup_fonts(ctx: &egui::Context) {
    for path in KO_FONT_CANDIDATES {
        if let Ok(bytes) = std::fs::read(path) {
            let mut fonts = egui::FontDefinitions::default();
            fonts.font_data.insert("ko".to_owned(), egui::FontData::from_owned(bytes).into());
            fonts.families.entry(egui::FontFamily::Proportional).or_default().insert(0, "ko".to_owned());
            fonts.families.entry(egui::FontFamily::Monospace).or_default().insert(0, "ko".to_owned());
            ctx.set_fonts(fonts);
            return;
        }
    }
}

fn main() -> Result<(), eframe::Error> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 800.0])
            .with_title("Hostmover")
            .with_app_id("hostmover"),
        ..Default::default()
    };
    eframe::run_native(
        "Hostmover",
        native_options,
        Box::new(|cc| {
            setup_fonts(&cc.egui_ctx);
            Ok(Box::new(app::App::new()))
        }),
    )
}
