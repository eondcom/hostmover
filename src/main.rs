mod app;
mod crypto;
mod model;
mod ops;
mod store;

const KO_REGULAR: &[&str] = &[
    "/usr/share/fonts/truetype/nanum/NanumGothic.ttf",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/truetype/nanum/NanumGothicBold.ttf",
];
const KO_BOLD: &[&str] = &[
    "/usr/share/fonts/truetype/nanum/NanumGothicBold.ttf",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc",
    "/usr/share/fonts/truetype/nanum/NanumGothic.ttf",
];

fn first_readable(paths: &[&str]) -> Option<Vec<u8>> {
    paths.iter().find_map(|p| std::fs::read(p).ok())
}

/// 본문=레귤러(Proportional/Monospace), 제목=볼드(Name("bold")) 로 분리 등록.
fn setup_fonts(ctx: &egui::Context) {
    let Some(reg) = first_readable(KO_REGULAR) else { return };
    let bold = first_readable(KO_BOLD).unwrap_or_else(|| reg.clone());
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert("ko".to_owned(), egui::FontData::from_owned(reg).into());
    fonts.font_data.insert("ko_bold".to_owned(), egui::FontData::from_owned(bold).into());
    fonts.families.entry(egui::FontFamily::Proportional).or_default().insert(0, "ko".to_owned());
    fonts.families.entry(egui::FontFamily::Monospace).or_default().insert(0, "ko".to_owned());
    fonts
        .families
        .insert(egui::FontFamily::Name("bold".into()), vec!["ko_bold".to_owned(), "ko".to_owned()]);
    ctx.set_fonts(fonts);
}

/// 다크 + 네이비/블루 액센트 테마 (금융권 대시보드 톤).
fn setup_theme(ctx: &egui::Context) {
    use egui::{Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, TextStyle};
    let rgb = Color32::from_rgb;
    let bg = rgb(0x0E, 0x14, 0x22); // 최상위 배경 (네이비 다크, 완전 검정 아님)
    let panel = rgb(0x14, 0x1C, 0x2E); // 패널
    let surface = rgb(0x1F, 0x2A, 0x44); // 카드/입력 배경 (패널보다 밝게 → 섹션 구분)
    let surface2 = rgb(0x29, 0x37, 0x55); // 버튼/호버 베이스
    let border = rgb(0x39, 0x4A, 0x6D); // 또렷한 경계
    let accent = rgb(0x3B, 0x82, 0xF6); // 블루 포인트
    let text = rgb(0xE5, 0xEB, 0xF5);
    let muted = rgb(0x93, 0xA1, 0xBC);

    let bold = FontFamily::Name("bold".into());
    let mut style = (*ctx.style()).clone();
    style.text_styles = [
        (TextStyle::Heading, FontId::new(18.0, bold.clone())),
        (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
        (TextStyle::Button, FontId::new(14.0, FontFamily::Proportional)),
        (TextStyle::Monospace, FontId::new(12.5, FontFamily::Monospace)),
        (TextStyle::Small, FontId::new(11.5, FontFamily::Proportional)),
    ]
    .into();
    // 컨트롤 높이 통일(34) + 적당한 여백
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(12.0, 8.0);
    style.spacing.interact_size = egui::vec2(44.0, 34.0);
    style.spacing.window_margin = Margin::same(10);
    style.spacing.menu_margin = Margin::same(6);
    style.spacing.indent = 18.0;

    let radius = CornerRadius::same(6);
    let mut v = egui::Visuals::dark();
    v.override_text_color = Some(text);
    v.panel_fill = panel;
    v.window_fill = bg;
    v.faint_bg_color = surface;
    v.extreme_bg_color = rgb(0x0A, 0x10, 0x1C);
    v.window_corner_radius = CornerRadius::same(9);
    v.window_stroke = Stroke::new(1.0, border);
    v.window_shadow = egui::epaint::Shadow {
        offset: [0, 4],
        blur: 18,
        spread: 0,
        color: Color32::from_black_alpha(120),
    };
    v.popup_shadow = v.window_shadow;
    v.selection.bg_fill = rgb(0x1C, 0x33, 0x55);
    v.selection.stroke = Stroke::new(1.0, accent);
    v.hyperlink_color = rgb(0x6F, 0xB0, 0xFF);
    v.warn_fg_color = rgb(0xF5, 0xB5, 0x4B);
    v.error_fg_color = rgb(0xF2, 0x6D, 0x6D);

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = surface;
    w.noninteractive.weak_bg_fill = surface;
    w.noninteractive.bg_stroke = Stroke::new(1.0, border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, muted);
    w.noninteractive.corner_radius = radius;
    w.inactive.bg_fill = surface2;
    w.inactive.weak_bg_fill = surface2;
    w.inactive.bg_stroke = Stroke::new(1.0, border);
    w.inactive.fg_stroke = Stroke::new(1.0, text);
    w.inactive.corner_radius = radius;
    w.hovered.bg_fill = rgb(0x1B, 0x29, 0x40);
    w.hovered.weak_bg_fill = rgb(0x1B, 0x29, 0x40);
    w.hovered.bg_stroke = Stroke::new(1.0, accent);
    w.hovered.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    w.hovered.corner_radius = radius;
    w.active.bg_fill = accent;
    w.active.weak_bg_fill = accent;
    w.active.bg_stroke = Stroke::new(1.0, accent);
    w.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    w.active.corner_radius = radius;
    w.open.bg_fill = surface;
    w.open.weak_bg_fill = surface;
    w.open.bg_stroke = Stroke::new(1.0, border);
    w.open.fg_stroke = Stroke::new(1.0, text);
    w.open.corner_radius = radius;

    style.visuals = v;
    ctx.set_style(style);
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
            setup_theme(&cc.egui_ctx);
            Ok(Box::new(app::App::new()))
        }),
    )
}
