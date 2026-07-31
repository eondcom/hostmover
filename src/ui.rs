//! 디자인 토큰과 공통 위젯.
//! 화면 코드는 의미 기반 토큰을 사용해 같은 상태를 항상 같은 방식으로 표시한다.

/// 의미 기반 색. `setup_theme()`의 기존 팔레트와 같은 값이다.
pub mod color {
    use egui::Color32;

    pub const OK: Color32 = Color32::from_rgb(0x2E, 0x9E, 0x5B);
    pub const WARN: Color32 = Color32::from_rgb(0xF5, 0xB5, 0x4B);
    pub const DANGER: Color32 = Color32::from_rgb(0xF2, 0x6D, 0x6D);
    pub const ACCENT: Color32 = Color32::from_rgb(0x3B, 0x82, 0xF6);
    pub const MUTED: Color32 = Color32::from_rgb(0x93, 0xA1, 0xBC);
    pub const BORDER: Color32 = Color32::from_rgb(0x39, 0x4A, 0x6D);
}

/// 화면 간격은 이 4의 배수 토큰만 사용한다.
pub mod space {
    pub const XS: f32 = 4.0;
    pub const SM: f32 = 8.0;
    pub const MD: f32 = 12.0;
    pub const LG: f32 = 16.0;
    #[allow(dead_code)]
    pub const XL: f32 = 24.0;
}

/// 표준 표 높이.
pub const TABLE_ROW_HEIGHT: f32 = 22.0;
pub const TABLE_HEADER_HEIGHT: f32 = 24.0;

/// 상태 배지. 색과 글자를 함께 보여 색각에 의존하지 않는다.
pub fn badge(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    egui::Frame::new()
        .fill(color.linear_multiply(0.25))
        .corner_radius(4.0)
        .inner_margin(egui::Margin::symmetric(6, 2))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).color(color));
        });
}

/// 상태 신호등 점.
pub fn dot(ui: &mut egui::Ui, color: egui::Color32) {
    ui.label(egui::RichText::new("●").color(color));
}

/// 대시보드용 수치와 라벨.
pub fn stat(ui: &mut egui::Ui, value: &str, label: &str, color: Option<egui::Color32>) {
    ui.vertical(|ui| {
        let text = egui::RichText::new(value)
            .family(egui::FontFamily::Name("bold".into()))
            .size(18.0);
        ui.label(color.map_or(text.clone(), |c| text.color(c)));
        ui.label(egui::RichText::new(label).small().color(self::color::MUTED));
    });
}

/// 섹션 제목, 우측 액션, 본문을 같은 간격으로 배치한다.
pub fn section<R>(
    ui: &mut egui::Ui,
    title: &str,
    actions: impl FnOnce(&mut egui::Ui),
    body: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(title).family(egui::FontFamily::Name("bold".into())),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), actions);
    });
    ui.add_space(space::XS);
    body(ui)
}

/// 빈 이유와 다음 행동을 함께 보여주는 표준 빈 상태.
pub fn empty_state(ui: &mut egui::Ui, icon: &str, message: &str, hint: Option<&str>) {
    ui.vertical_centered(|ui| {
        ui.add_space(space::SM);
        ui.label(egui::RichText::new(icon).size(18.0).color(color::MUTED));
        ui.label(message);
        if let Some(hint) = hint {
            ui.label(egui::RichText::new(hint).small().color(color::MUTED));
        }
        ui.add_space(space::SM);
    });
}

/// 숫자 셀: 고정폭 글꼴과 우측 정렬.
pub fn num(ui: &mut egui::Ui, text: &str) {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.label(egui::RichText::new(text).monospace());
    });
}

/// 정렬 헤더. 같은 열을 다시 누르면 방향을 바꾼다.
pub fn sort_header(ui: &mut egui::Ui, label: &str, column: usize, state: &mut SortState) {
    let marker = if state.col == column {
        if state.desc { " ▼" } else { " ▲" }
    } else {
        ""
    };
    if ui.button(format!("{label}{marker}")).clicked() {
        if state.col == column {
            state.desc = !state.desc;
        } else {
            state.col = column;
            state.desc = false;
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SortState {
    pub col: usize,
    pub desc: bool,
}

impl SortState {
    pub const fn new(col: usize, desc: bool) -> Self {
        Self { col, desc }
    }
}
