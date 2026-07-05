use crate::model::{ActivityLog, CachedSite, CmsAccess, CmsKind, Customer, Domain, DomainAccess, Site, Store};
use crate::ops::{self, LogMsg, OpKind};
use crate::store;
use egui_phosphor::regular as ph;
use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};

/// 묶음 이전 종류
#[derive(Clone, Copy, PartialEq)]
enum MigrateKind {
    /// DB백업 → 파일백업 → DB복원 → 파일복원
    Full,
    /// 파일백업 → 파일복원
    FilesOnly,
    /// DB백업 → DB복원
    DbOnly,
    /// DB직접 → 파일직접 (로컬 디스크 미사용)
    Direct,
}

/// 실행 요청 종류
#[derive(Clone, Copy, PartialEq)]
enum Req {
    Op(OpKind),
    Migrate(MigrateKind),
}

/// 실행할 작업 묶음 (요청, 고객명, 도메인명, 현재사이트, 신규사이트)
type PendingAction = (Req, String, String, Site, Site);

/// 명령어 보기 모달 내용
#[derive(Clone)]
struct CmdView {
    title: String,
    command: String,
}

/// 도메인 작업영역 탭
#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Info,
    Migrate,
    Cms,
    Eond,
    History,
}

/// 중앙 영역에 표시할 화면
#[derive(Clone, Copy, PartialEq)]
enum MainView {
    Domain,
    Settings,
    /// 계정별 Rhymix 모듈 관리 (고객 인덱스)
    AccountModules(usize),
    /// 전체 사이트(모든 계정) 통합 리스트
    AllSites,
}

/// 설정 페이지 내부 탭
#[derive(Clone, Copy, PartialEq)]
enum SettingsTab {
    Connect,
    Ssh,
    BulkUpdate,
    ModuleDelete,
    Disk,
    Backup,
}

/// 계정 페이지 내부 탭
#[derive(Clone, Copy, PartialEq)]
enum AcctTab {
    Sites,
    Modules,
}

/// 계정/전체 사이트 스캔 행
struct AcctSiteRow {
    account: String,
    domain: String,
    kind: String,
    version: String,
    status: String,
    git: bool,
    file_bytes: u64,
    db_bytes: u64,
    /// DNS A 레코드 (DNS 체크 시 채움)
    a_record: String,
    /// HestiaCP 도메인 alias (None=미조회). 보이면 자동 조회.
    aliases: Option<Vec<String>>,
    /// alias 조회 요청됨(중복 방지)
    alias_req: bool,
    sel: bool,
}

impl AcctSiteRow {
    fn new(account: String, domain: String, kind: String, version: String, status: String, git: bool, file_bytes: u64, db_bytes: u64) -> Self {
        AcctSiteRow { account, domain, kind, version, status, git, file_bytes, db_bytes, a_record: String::new(), aliases: None, alias_req: false, sel: false }
    }
}

/// 백그라운드 스캔 결과 (UI 스레드로 전달)
enum ScanMsg {
    AllSites { res: Result<Vec<(String, String, String, String, String, bool, u64, u64)>, String> },
    /// 계정 한 곳만 재스캔해 전체 리스트에 병합
    AccountIntoAll { account: String, res: Result<Vec<(String, String, String, String, bool, u64, u64)>, String> },
    /// 선택 사이트만 재스캔해 해당 행만 갱신 (업데이트 직후)
    SelectedSites { res: Result<Vec<(String, String, String, String, String, bool, u64, u64)>, String> },
    AccountModules { ci: usize, label: String, res: Result<Vec<(String, Vec<String>)>, String> },
    /// DNS A 레코드 조회 결과 (도메인, A값)
    Dns { results: Vec<(String, String)> },
    /// 도메인 alias 일괄 조회 결과 (계정, 도메인, alias목록)
    AliasesBatch { results: Vec<(String, String, Vec<String>)> },
    /// WordPress 플러그인/테마 버전 비교 스캔 결과 (도메인 id, 종류, 행들)
    WpPlugins { domain_id: u64, kind: ops::WpAssetKind, res: Result<Vec<ops::WpPluginRow>, String> },
}

/// 사이트 표의 컬럼: (텍스트, 색, 폭)
type SiteCols = Vec<(String, egui::Color32, f32)>;
const SITE_LEAD: f32 = 30.0; // 체크 글리프 영역 폭

/// 한 행 전체(여백 포함)를 클릭 영역으로 그린다. 체크 글리프 + 고정폭 컬럼. 클릭되면 true.
fn site_row(ui: &mut egui::Ui, selected: bool, cols: &SiteCols) -> bool {
    let need: f32 = SITE_LEAD + cols.iter().map(|c| c.2).sum::<f32>();
    let w = ui.available_width().max(need);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 22.0), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let vis = ui.visuals();
        if selected {
            ui.painter().rect_filled(rect, 3.0, vis.selection.bg_fill);
        } else if resp.hovered() {
            ui.painter().rect_filled(rect, 3.0, vis.widgets.hovered.weak_bg_fill);
        }
        let cy = rect.center().y;
        let chk = if selected { ph::CHECK_SQUARE } else { ph::SQUARE };
        let chk_col = if selected { vis.selection.stroke.color } else { vis.weak_text_color() };
        ui.painter().text(egui::pos2(rect.left() + 7.0, cy), egui::Align2::LEFT_CENTER, chk, egui::FontId::proportional(15.0), chk_col);
        let mut x = rect.left() + SITE_LEAD;
        for (txt, col, wdt) in cols {
            ui.painter().text(egui::pos2(x, cy), egui::Align2::LEFT_CENTER, txt.as_str(), egui::FontId::proportional(13.0), *col);
            x += *wdt;
        }
    }
    resp.clicked()
}

/// 설치 버전 vs 최신 버전 비교 → 상태 문자열
fn compute_status(kind: &str, version: &str, git: bool, rx: &str, wp: &str, gn: &str) -> String {
    if !version.contains('.') {
        return if git { "확인필요".into() } else { "-".into() };
    }
    let cmp = |latest: &str| if latest.is_empty() { "-".into() } else if version == latest { "최신버전".into() } else { format!("업데이트필요 ({latest})") };
    match kind {
        "Rhymix" => cmp(rx),
        "WordPress" => cmp(wp),
        "Gnuboard" => cmp(gn),
        _ => "-".into(),
    }
}

fn kind_color(kind: &str, git: bool) -> egui::Color32 {
    if kind.starts_with('(') || kind == "unknown" { egui::Color32::GRAY }
    else if git { egui::Color32::from_rgb(80, 170, 110) }
    else { egui::Color32::from_rgb(150, 150, 200) }
}

fn status_color(s: &str) -> egui::Color32 {
    if s.starts_with("최신") { egui::Color32::from_rgb(80, 180, 110) }
    else if s.starts_with("업데이트") { egui::Color32::from_rgb(220, 150, 60) }
    else if s == "확인필요" { egui::Color32::from_rgb(200, 190, 90) }
    else { egui::Color32::GRAY }
}

/// 헤더/합계 등 클릭 없는 행 (체크 영역만큼 들여쓰기)
fn site_info_row(ui: &mut egui::Ui, cols: &SiteCols) {
    let need: f32 = SITE_LEAD + cols.iter().map(|c| c.2).sum::<f32>();
    let w = ui.available_width().max(need);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 22.0), egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        let cy = rect.center().y;
        let mut x = rect.left() + SITE_LEAD;
        for (txt, col, wdt) in cols {
            ui.painter().text(egui::pos2(x, cy), egui::Align2::LEFT_CENTER, txt.as_str(), egui::FontId::proportional(13.0), *col);
            x += *wdt;
        }
    }
}

/// 경과 시간 한국어 표기
fn ago_text(at: i64) -> String {
    if at <= 0 { return "시각 미상".into(); }
    let d = (now_unix() - at).max(0);
    if d < 60 { "방금".into() }
    else if d < 3600 { format!("{}분 전", d / 60) }
    else if d < 86400 { format!("{}시간 전", d / 3600) }
    else { format!("{}일 전", d / 86400) }
}

/// 바이트를 사람이 읽기 쉬운 단위로
fn human_bytes(n: u64) -> String {
    if n == 0 { return "-".into(); }
    let units = ["B", "K", "M", "G", "T"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < units.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{}{}", n, units[0]) } else { format!("{:.1}{}", v, units[i]) }
}

/// Rhymix 지원종료(삭제 권장) 모듈 — 안내 표시용. 필요 시 추가.
const DEPRECATED_MODULES: &[&str] = &["seo", "auto_login", "trackback"];

fn is_deprecated_module(name: &str) -> bool {
    DEPRECATED_MODULES.contains(&name.trim())
}

/// 휴지통 보관 기간 (30일)
const TRASH_RETENTION_SECS: i64 = 30 * 24 * 3600;

fn now_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// 삭제 시각으로부터 완전삭제까지 남은 일수
fn remaining_days(deleted_at: i64) -> i64 {
    let elapsed = now_unix() - deleted_at;
    ((TRASH_RETENTION_SECS - elapsed) / (24 * 3600)).max(0)
}

/// 삭제 확인 대상
#[derive(Clone, Copy)]
enum DelTarget {
    Customer(usize),
    Domain(usize, usize),
}

/// 락 화면 입력/버튼 공통 치수
const LOCK_W: f32 = 300.0;
const LOCK_H: f32 = 34.0;
const LOCK_PAD: egui::Vec2 = egui::vec2(14.0, 9.0);
/// 일반 입력칸 내부 패딩 — 버튼(높이 34)과 높이를 맞춘다
const FIELD_MARGIN: egui::Vec2 = egui::vec2(8.0, 9.0);
/// 그리드 라벨 칸 고정 폭 — 입력칸 시작점을 섹션 간 정렬
const LABEL_W: f32 = 116.0;

/// unix초 → "YYYY-MM-DD HH:MM" (KST, +9h)
fn fmt_kst(ts: i64) -> String {
    let t = ts + 9 * 3600;
    let days = t.div_euclid(86400);
    let secs = t.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", secs / 3600, (secs % 3600) / 60)
}

/// unix초 → "YYYY-MM-DD" (KST). 0 이하면 "-".
fn short_date(ts: i64) -> String {
    if ts <= 0 { return "-".into(); }
    fmt_kst(ts).split(' ').next().unwrap_or("-").to_string()
}

/// days(에포크 기준) → (년,월,일). Howard Hinnant 알고리즘.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// 디자인 토큰 (색)
const ACCENT: egui::Color32 = egui::Color32::from_rgb(0x3B, 0x82, 0xF6);
const C_GREEN: egui::Color32 = egui::Color32::from_rgb(0x2E, 0x9E, 0x5B);
const C_RED: egui::Color32 = egui::Color32::from_rgb(0xC4, 0x5A, 0x5A);

/// 주(primary) 액션 버튼 — 액센트 채움 + 흰 글씨
fn btn_primary(label: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(label).color(egui::Color32::WHITE).strong()).fill(ACCENT)
}
/// 긍정(go) 버튼 — 녹색 채움
fn btn_go(label: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(label).color(egui::Color32::WHITE).strong()).fill(C_GREEN)
}
/// 위험(danger) 버튼 — 빨강 채움
fn btn_danger(label: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(label).color(egui::Color32::WHITE)).fill(C_RED)
}

/// 채움색 + 테두리를 가진 섹션 카드 (배경과 또렷이 구분)
fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::group(ui.style())
        .fill(ui.visuals().faint_bg_color)
        .inner_margin(egui::Margin::same(10))
        .show(ui, add)
        .inner
}

/// 고정 폭 라벨 셀 (그리드 정렬용)
fn grid_label(ui: &mut egui::Ui, text: &str) {
    let h = ui.spacing().interact_size.y;
    ui.allocate_ui_with_layout(
        egui::vec2(LABEL_W, h),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.label(text);
        },
    );
}

/// 가운데 정렬 placeholder 를 갖는 마스터 패스워드 입력칸
fn pw_field<'a>(value: &'a mut String, hint: &str, visible: bool) -> egui::TextEdit<'a> {
    egui::TextEdit::singleline(value)
        .password(!visible)
        .hint_text(hint)
        .desired_width(LOCK_W)
        .horizontal_align(egui::Align::Center)
        .margin(LOCK_PAD)
}

pub struct App {
    locked: bool,
    password: String,
    password_confirm: String,
    creating: bool,
    auth_error: String,

    store: Store,
    master_pw: String,
    dirty: bool,
    status: String,
    last_ok: Option<bool>,

    sel_customer: Option<usize>,
    sel_domain: Option<usize>,
    tab: Tab,
    /// 직전에 렌더한 도메인 탭 — CMS/eondcms 탭 "진입 시" 루트 자동체크를 위해 추적(복원/도메인이동 진입 포함)
    prev_domain_tab: Option<Tab>,
    show_pw: bool,
    show_lock_pw: bool,
    use_root: bool,

    new_customer: String,
    /// 고객별 "새 도메인" 입력 버퍼 (고객id → 입력중 문자열). 공유 입력 버그 방지.
    new_domain: HashMap<u64, String>,
    /// ➕ 로 새 도메인 입력칸을 펼친 고객 id 집합
    add_open: std::collections::HashSet<u64>,
    /// 접힌 고객 id 집합 (기본 펼침)
    collapsed: std::collections::HashSet<u64>,
    /// 휴지통 보기 모드
    show_trash: bool,
    /// 삭제 확인 모달 대상
    pending_delete: Option<DelTarget>,
    /// 실행 중인 작업 (도메인id, 제목) — 완료 시 해당 도메인 기록에 추가
    running_job: Option<(u64, String)>,
    /// 중앙 영역 화면 전환
    view: MainView,
    /// 설정 페이지 내부 탭
    settings_tab: SettingsTab,
    /// 계정 페이지 내부 탭
    acct_tab: AcctTab,
    /// 전체 사이트(모든 계정) 스캔 결과 — 계정 관리도 이 캐시를 필터링해 공유
    all_sites: Vec<AcctSiteRow>,
    /// 전체 사이트 화면 상태 메시지
    all_sites_status: String,
    /// 전체 사이트: 계정 필터 ("" = 전체)
    all_filter_acct: String,
    /// 전체 사이트: 도메인 검색어
    all_search: String,
    /// 계정별 모듈 목록: (모듈명, 사용 도메인들, 선택여부)
    acct_mods: Vec<(String, Vec<String>, bool)>,
    /// acct_mods/acct_sites 가 어느 고객 것인지
    acct_mods_for: Option<usize>,
    /// 모듈 작업 도메인 범위 ("" = 계정 전체, 아니면 특정 도메인)
    acct_mod_domain: String,
    /// 계정 모듈 화면 상태 메시지
    acct_mods_status: String,
    /// 사이트 불러오기 요청 (고객 인덱스)
    pending_import_sites: Option<usize>,

    log: Vec<String>,
    running: bool,
    confirm: Option<PendingAction>,
    cmd_view: Option<CmdView>,
    eond_confirm: Option<ops::Job>,
    last_edit: f64,
    /// 설정의 Rhymix 모듈 삭제 도구 입력값
    mod_del_name: String,
    mod_del_acct: String,
    disk_path: String,
    disk_alert_email: String,
    /// WordPress 플러그인/테마 버전비교 스캔 결과 캐시 — (도메인id, 종류) → 행들.
    /// 재방문·플러그인↔테마 전환 시 네트워크 재조회 없이 즉시 표시(스캔 버튼=강제 새로고침).
    wp_cache: std::collections::HashMap<(u64, ops::WpAssetKind), Vec<ops::WpPluginRow>>,
    wp_sel: std::collections::HashSet<String>,
    /// 현재 스캔/업로드 대상 종류 (플러그인/테마)
    wp_kind: ops::WpAssetKind,
    /// 플러그인/테마 표 검색어(이름·slug 부분일치 필터)
    wp_filter: String,
    wp_scanning: bool,
    wp_scan_status: String,
    /// 복원할 백업 파일 경로(직접 입력) + 덮어쓰기 확인
    restore_path: String,
    restore_armed: bool,

    tx: Sender<LogMsg>,
    rx: Receiver<LogMsg>,
    /// 백그라운드 스캔 채널 + 진행 플래그
    scan_tx: Sender<ScanMsg>,
    scan_rx: Receiver<ScanMsg>,
    scanning: bool,
    /// 업데이트 실행 후 자동 재스캔할 (계정,도메인) — 작업 완료 시 소비
    pending_rescan: Vec<(String, String)>,
    /// 작업 완료가 감지되어 pending_rescan 실행 대기
    pending_rescan_ready: bool,
}

impl App {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        let (scan_tx, scan_rx) = channel();
        Self {
            locked: true,
            password: String::new(),
            password_confirm: String::new(),
            creating: !store::exists(),
            auth_error: String::new(),
            store: Store::default(),
            master_pw: String::new(),
            dirty: false,
            status: String::new(),
            last_ok: None,
            sel_customer: None,
            sel_domain: None,
            tab: Tab::Info,
            prev_domain_tab: None,
            show_pw: false,
            show_lock_pw: false,
            use_root: false,
            new_customer: String::new(),
            new_domain: HashMap::new(),
            add_open: std::collections::HashSet::new(),
            collapsed: std::collections::HashSet::new(),
            show_trash: false,
            pending_delete: None,
            running_job: None,
            view: MainView::Domain,
            settings_tab: SettingsTab::Connect,
            acct_tab: AcctTab::Sites,
            all_sites: Vec::new(),
            all_sites_status: String::new(),
            all_filter_acct: String::new(),
            all_search: String::new(),
            acct_mods: Vec::new(),
            acct_mods_for: None,
            acct_mod_domain: String::new(),
            acct_mods_status: String::new(),
            pending_import_sites: None,
            log: Vec::new(),
            running: false,
            confirm: None,
            cmd_view: None,
            eond_confirm: None,
            last_edit: 0.0,
            mod_del_name: String::new(),
            mod_del_acct: String::new(),
            disk_path: "/backup".to_string(),
            disk_alert_email: "eond@eond.com".to_string(),
            wp_cache: std::collections::HashMap::new(),
            wp_sel: std::collections::HashSet::new(),
            wp_kind: ops::WpAssetKind::Plugin,
            wp_filter: String::new(),
            wp_scanning: false,
            wp_scan_status: String::new(),
            restore_path: String::new(),
            restore_armed: false,
            tx,
            rx,
            scan_tx,
            scan_rx,
            scanning: false,
            pending_rescan: Vec::new(),
            pending_rescan_ready: false,
        }
    }

    fn save(&mut self) {
        self.snapshot_ui();
        match store::save(&self.master_pw, &self.store) {
            Ok(_) => {
                self.dirty = false;
                self.status = "저장 완료".into();
            }
            Err(e) => self.status = format!("저장 실패: {e}"),
        }
    }

    /// 현재 화면 위치를 UiState 로 (재시작 복원용).
    fn current_ui(&self) -> crate::model::UiState {
        let view = match self.view {
            MainView::Domain => "domain",
            MainView::Settings => "settings",
            MainView::AllSites => "allsites",
            MainView::AccountModules(_) => "account",
        };
        let tab = match self.tab {
            Tab::Info => "info", Tab::Migrate => "migrate", Tab::Cms => "cms", Tab::Eond => "eond", Tab::History => "history",
        };
        let settings_tab = match self.settings_tab {
            SettingsTab::Connect => "connect", SettingsTab::Ssh => "ssh", SettingsTab::BulkUpdate => "bulk",
            SettingsTab::ModuleDelete => "moddel", SettingsTab::Disk => "disk", SettingsTab::Backup => "backup",
        };
        crate::model::UiState {
            view: view.into(),
            customer_id: self.sel_customer.and_then(|ci| self.store.customers.get(ci)).map(|c| c.id).unwrap_or(0),
            domain_id: self.current_domain_id().unwrap_or(0),
            tab: tab.into(),
            settings_tab: settings_tab.into(),
        }
    }

    /// 현재 화면 위치를 store.ui 에 기록 (save() 시 호출).
    fn snapshot_ui(&mut self) {
        self.store.ui = self.current_ui();
    }

    /// store.ui 의 마지막 화면 위치를 복원 (잠금 해제 직후 호출). id→index 로 해석해 정렬/추가에도 안정적.
    fn restore_ui(&mut self) {
        let ui = self.store.ui.clone();
        if ui.customer_id != 0 {
            if let Some(ci) = self.store.customers.iter().position(|c| c.id == ui.customer_id && c.deleted_at.is_none()) {
                self.sel_customer = Some(ci);
                if ui.domain_id != 0 {
                    if let Some(di) = self.store.customers[ci].domains.iter().position(|d| d.id == ui.domain_id && d.deleted_at.is_none()) {
                        self.sel_domain = Some(di);
                    }
                }
            }
        }
        self.tab = match ui.tab.as_str() {
            "migrate" => Tab::Migrate, "cms" => Tab::Cms, "eond" => Tab::Eond, "history" => Tab::History, _ => Tab::Info,
        };
        self.settings_tab = match ui.settings_tab.as_str() {
            "ssh" => SettingsTab::Ssh, "bulk" => SettingsTab::BulkUpdate, "moddel" => SettingsTab::ModuleDelete,
            "disk" => SettingsTab::Disk, "backup" => SettingsTab::Backup, _ => SettingsTab::Connect,
        };
        self.view = match ui.view.as_str() {
            "settings" => MainView::Settings,
            "allsites" => MainView::AllSites,
            "account" => self.sel_customer.map(MainView::AccountModules).unwrap_or(MainView::Domain),
            _ => MainView::Domain,
        };
    }

    /// 현재 선택된 도메인의 id
    fn current_domain_id(&self) -> Option<u64> {
        let c = self.sel_customer?;
        let d = self.sel_domain?;
        self.store.customers.get(c)?.domains.get(d).map(|dom| dom.id)
    }

    /// id 로 도메인 가변 참조 검색
    fn find_domain_mut(&mut self, id: u64) -> Option<&mut Domain> {
        for c in &mut self.store.customers {
            for d in &mut c.domains {
                if d.id == id {
                    return Some(d);
                }
            }
        }
        None
    }

    fn hestia_test(&mut self) {
        match ops::hestia_check(&self.store.settings) {
            Ok(n) => {
                self.last_ok = Some(true);
                self.status = format!("HestiaCP 연결 OK — 유저 {n}명");
                self.log.push(format!("HestiaCP 연결 성공 (유저 {n}명)"));
            }
            Err(e) => {
                self.last_ok = Some(false);
                self.status = format!("HestiaCP 연결 실패: {e}");
                self.log.push(format!("HestiaCP 연결 실패: {e}"));
            }
        }
    }

    /// HestiaCP 유저 → 고객으로 불러오기 (이름 중복 제외)
    fn hestia_import_users(&mut self) {
        match ops::hestia_list_users(&self.store.settings) {
            Ok(users) => {
                let mut added = 0;
                for u in users {
                    let exists = self.store.customers.iter().any(|c| c.deleted_at.is_none() && c.name == u);
                    if !exists {
                        let id = self.store.alloc_id();
                        self.store.customers.push(Customer {
                            id,
                            name: u,
                            memo: String::new(),
                            domains: Vec::new(),
                            deleted_at: None,
                        });
                        added += 1;
                    }
                }
                self.last_ok = Some(true);
                self.status = format!("고객(유저) {added}명 불러옴");
                self.log.push(format!("HestiaCP 고객 불러오기: {added}명 추가"));
                self.dirty = true;
                self.save();
            }
            Err(e) => {
                self.last_ok = Some(false);
                self.status = format!("고객 불러오기 실패: {e}");
                self.log.push(format!("고객 불러오기 실패: {e}"));
            }
        }
    }

    /// 고객(=HestiaCP 유저)의 웹도메인 → 도메인으로 불러오기 (IP/hestia유저 자동 채움)
    fn hestia_import_sites(&mut self, ci: usize) {
        let user = match self.store.customers.get(ci) {
            Some(c) => c.name.clone(),
            None => return,
        };
        match ops::hestia_list_web_domains(&self.store.settings, &user) {
            Ok(items) => {
                let mut added = 0;
                for (dom, ip) in items {
                    let exists = self.store.customers[ci].domains.iter().any(|d| d.deleted_at.is_none() && d.name == dom);
                    if exists {
                        continue;
                    }
                    let id = self.store.alloc_id();
                    let mut nd = Domain {
                        id,
                        name: dom,
                        memo: String::new(),
                        access: DomainAccess::default(),
                        asis: Site::default(),
                        tobe: Site::default(),
                        cms: CmsAccess::default(),
                        eond: Default::default(),
                        cms_install: Default::default(),
                        deleted_at: None,
                        history: Vec::new(),
                    };
                    nd.tobe.ip = ip;
                    // HestiaCP 유저 = vhost FTP 계정 → 정보 탭(신규 서버)의 FTP ID로도 채움
                    nd.tobe.ftp_id = user.clone();
                    nd.eond.hestia_user = user.clone();
                    nd.cms_install.hestia_user = user.clone();
                    self.store.customers[ci].domains.push(nd);
                    added += 1;
                }
                // CMS 자동 감지 (서버 SSH 설정 시): 종류를 cms_install.kind 에 반영, 미지원 종류는 메모에 기록
                if !self.store.settings.ssh_user.trim().is_empty() {
                    match ops::scan_account_sites(&self.store.settings, &user) {
                        Ok(scan) => {
                            let mut det = 0;
                            for (dom, kind, version, _status, _git, _fb, _db) in scan {
                                if let Some(d) = self.store.customers[ci].domains.iter_mut().find(|d| d.deleted_at.is_none() && d.name == dom) {
                                    match kind.as_str() {
                                        "WordPress" => d.cms_install.kind = CmsKind::WordPress,
                                        "Rhymix" => d.cms_install.kind = CmsKind::Rhymix,
                                        "Gnuboard" => d.cms_install.kind = CmsKind::Gnuboard,
                                        _ => {}
                                    }
                                    if kind != "unknown" {
                                        let tag = format!("[감지: {kind} {version}]");
                                        if !d.memo.contains("[감지:") {
                                            d.memo = if d.memo.trim().is_empty() { tag } else { format!("{} {tag}", d.memo) };
                                        }
                                        det += 1;
                                    }
                                }
                            }
                            self.log.push(format!("CMS 자동 감지 [{user}]: {det}개"));
                        }
                        Err(e) => { self.log.push(format!("CMS 감지 건너뜀 [{user}]: {e}")); }
                    }
                }
                self.last_ok = Some(true);
                self.status = format!("{user}: 사이트 {added}개 불러옴");
                self.log.push(format!("HestiaCP 사이트 불러오기 [{user}]: {added}개 추가"));
                self.dirty = true;
                self.save();
            }
            Err(e) => {
                self.last_ok = Some(false);
                self.status = format!("사이트 불러오기 실패: {e}");
                self.log.push(format!("사이트 불러오기 실패 [{user}]: {e}"));
            }
        }
    }

    /// ⚙ 설정 페이지 (탭: HestiaCP 연동 / 서버 SSH / 일괄 업데이트 / 모듈 일괄삭제)
    fn settings_page(&mut self, ctx: &egui::Context) {
        let mut show_pw = self.show_pw;
        let mut do_test = false;
        let mut do_import_users = false;
        let mut do_panel_probe = false;
        let mut do_panel_diag = false;
        let mut do_panel_recover = false;
        let mut do_disk_health = false;
        let mut do_disk_scrub: Option<u32> = None;
        let mut do_disk_monitor: Option<bool> = None;
        let mut do_bulk_update: Option<bool> = None;
        let mut do_module_delete: Option<bool> = None;
        let mut do_backup = false;
        let mut do_restore: Option<std::path::PathBuf> = None;
        let mut close = false;
        let running = self.running;
        let frame = egui::Frame::central_panel(&ctx.style()).inner_margin(egui::Margin::symmetric(14, 12));
        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading(format!("{}  설정", ph::GEAR));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(format!("{}  도메인으로", ph::X)).clicked() { close = true; }
                });
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                for (t, label) in [
                    (SettingsTab::Connect, "  HestiaCP 연동  "),
                    (SettingsTab::Ssh, "  서버 SSH  "),
                    (SettingsTab::BulkUpdate, "  일괄 업데이트  "),
                    (SettingsTab::ModuleDelete, "  모듈 일괄 삭제  "),
                    (SettingsTab::Disk, "  디스크 점검  "),
                    (SettingsTab::Backup, "  백업/복원  "),
                ] {
                    if ui.selectable_label(self.settings_tab == t, label).clicked() {
                        self.settings_tab = t;
                    }
                }
            });
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(10.0);
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                match self.settings_tab {
                    SettingsTab::Connect => {
                        card(ui, |ui| {
                            ui.strong("HestiaCP API 연동");
                            ui.add_space(4.0);
                            egui::Grid::new("settings_grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                                grid_label(ui, "호스트");
                                ui.add(egui::TextEdit::singleline(&mut self.store.settings.hestia_host).hint_text("HestiaCP IP/도메인").desired_width(260.0).margin(FIELD_MARGIN));
                                ui.end_row();
                                grid_label(ui, "포트");
                                ui.add(egui::TextEdit::singleline(&mut self.store.settings.hestia_port).hint_text("8083").desired_width(260.0).margin(FIELD_MARGIN));
                                ui.end_row();
                                grid_label(ui, "API 해시");
                                ui.horizontal(|ui| {
                                    ui.add(egui::TextEdit::singleline(&mut self.store.settings.hestia_hash).password(!show_pw).hint_text("관리자 > 서버설정 > API 접근키").desired_width(230.0).margin(FIELD_MARGIN));
                                    let (icon, tip) = if show_pw { (ph::EYE_SLASH, "비밀번호 숨기기") } else { (ph::EYE, "비밀번호 표시") };
                                    if ui.button(icon).on_hover_text(tip).clicked() { show_pw = !show_pw; }
                                });
                                ui.end_row();
                                grid_label(ui, "SSL 검증");
                                ui.checkbox(&mut self.store.settings.ssl_verify, "자체서명 인증서면 끄기");
                                ui.end_row();
                            });
                            ui.add_space(8.0);
                            ui.horizontal(|ui| {
                                if ui.button(format!("{}  연결 테스트", ph::PLUGS_CONNECTED)).clicked() { do_test = true; }
                                if ui.add(btn_primary(format!("{}  고객 불러오기", ph::USERS))).clicked() { do_import_users = true; }
                            });
                            ui.add_space(4.0);
                            ui.label(egui::RichText::new("사이트 불러오기는 좌측 고객 옆 📥 버튼으로 (고객별).").weak());
                            ui.label(egui::RichText::new("불러온 도메인은 신규(TOBE) IP + HestiaCP 유저가 자동 입력됩니다.").weak());
                        });
                        card(ui, |ui| {
                            ui.strong(format!("{}  웹패널 접속 진단", ph::STETHOSCOPE));
                            ui.label(egui::RichText::new("관리자 웹패널(:8083)이 안 열릴 때 원인을 찾습니다. 서브도메인 alias 추가 등으로 nginx 설정이 깨지면 hestia 서비스가 못 떠 포트가 닫힙니다.").weak());
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                if ui.button(format!("{}  네트워크 진단", ph::WIFI_HIGH))
                                    .on_hover_text("내 PC에서 패널 포트 도달성·HTTP·TLS·인증서 확인 (SSH 불필요)")
                                    .clicked() { do_panel_probe = true; }
                                if ui.add_enabled(!running, btn_primary(format!("{}  서버 진단 (SSH)", ph::STETHOSCOPE)))
                                    .on_hover_text("서버에 SSH로 들어가 hestia 서비스/포트/nginx 설정/에러로그/디스크 수집 — '서버 SSH' 설정 필요")
                                    .clicked() { do_panel_diag = true; }
                                if ui.add_enabled(!running, egui::Button::new(format!("{}  패널 복구", ph::WRENCH))
                                    .fill(egui::Color32::from_rgb(150, 80, 40)))
                                    .on_hover_text("포트는 열렸는데 페이지가 타임아웃될 때: 멈춘 통계 프로세스 정리 + hestia 재시작 (확인 후 실행)")
                                    .clicked() { do_panel_recover = true; }
                            });
                            ui.add_space(4.0);
                            ui.label(egui::RichText::new("서버 진단 결과는 아래 로그창에 표시됩니다. 진단은 읽기 전용이며, '패널 복구'는 확인 후 서버를 재시작합니다.").weak());
                        });
                    }
                    SettingsTab::Ssh => {
                        card(ui, |ui| {
                            ui.strong("서버 SSH (일괄 작업용 · sudo 경유)");
                            ui.label(egui::RichText::new("root 직접 로그인 대신 sudo 권한 계정(예: tong)으로 접속해 sudo로 root 권한 사용. 일괄 업데이트·모듈 삭제에 사용됩니다.").weak());
                            ui.add_space(6.0);
                            egui::Grid::new("settings_ssh_grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                                grid_label(ui, "SSH 호스트");
                                ui.add(egui::TextEdit::singleline(&mut self.store.settings.ssh_host).hint_text("비우면 HestiaCP 호스트 사용").desired_width(260.0).margin(FIELD_MARGIN));
                                ui.end_row();
                                grid_label(ui, "SSH 유저");
                                ui.add(egui::TextEdit::singleline(&mut self.store.settings.ssh_user).hint_text("sudo 권한 계정 (예: tong)").desired_width(260.0).margin(FIELD_MARGIN));
                                ui.end_row();
                                grid_label(ui, "SSH 비번");
                                ui.horizontal(|ui| {
                                    ui.add(egui::TextEdit::singleline(&mut self.store.settings.ssh_pass).password(!show_pw).hint_text("SSH + sudo 공용 비밀번호").desired_width(230.0).margin(FIELD_MARGIN));
                                    let (icon, tip) = if show_pw { (ph::EYE_SLASH, "비밀번호 숨기기") } else { (ph::EYE, "비밀번호 표시") };
                                    if ui.button(icon).on_hover_text(tip).clicked() { show_pw = !show_pw; }
                                });
                                ui.end_row();
                                grid_label(ui, "SSH 포트");
                                ui.add(egui::TextEdit::singleline(&mut self.store.settings.ssh_port).hint_text("22").desired_width(260.0).margin(FIELD_MARGIN));
                                ui.end_row();
                                grid_label(ui, "Rhymix 소스");
                                ui.add(egui::TextEdit::singleline(&mut self.store.settings.rx_source_local).hint_text("이 PC의 dev/rx 경로 (하위에 modules/ layouts/)").desired_width(260.0).margin(FIELD_MARGIN))
                                    .on_hover_text("로컬 모듈/레이아웃 소스 폴더. 도메인 화면의 Rhymix 모듈/레이아웃 업로드가 여기서 가져갑니다.");
                                ui.end_row();
                                grid_label(ui, "WordPress 소스");
                                ui.add(egui::TextEdit::singleline(&mut self.store.settings.wp_source_local).hint_text("이 PC의 dev/wp 경로 (하위에 wp-content/plugins/, themes/)").desired_width(260.0).margin(FIELD_MARGIN))
                                    .on_hover_text("로컬 플러그인/테마 소스 폴더. 도메인 화면(WordPress 선택)의 플러그인·테마 버전 비교/동기화가 여기서 가져갑니다.");
                                ui.end_row();
                            });
                        });
                    }
                    SettingsTab::BulkUpdate => {
                        card(ui, |ui| {
                            ui.strong(format!("{}  Rhymix/그누보드 일괄 업데이트", ph::ARROWS_CLOCKWISE));
                            ui.label(egui::RichText::new("서버의 모든 vhost 웹루트(/home/*/web/*/public_html)를 순회합니다. .git 있으면 얕은 업데이트(depth=1), 없으면 '수동/선택 필요'로 보고만 합니다.").weak());
                            ui.label(egui::RichText::new("※ 서버 SSH(sudo 유저) 설정이 필요합니다.").weak());
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                if ui.add_enabled(!running, btn_primary(format!("{}  일괄 업데이트 실행", ph::ARROWS_CLOCKWISE))).clicked() {
                                    do_bulk_update = Some(true);
                                }
                                if ui.button(format!("{}  명령어 보기", ph::FILE_TEXT)).clicked() {
                                    do_bulk_update = Some(false);
                                }
                            });
                        });
                    }
                    SettingsTab::ModuleDelete => {
                        card(ui, |ui| {
                            ui.strong(format!("{}  Rhymix 모듈 일괄 삭제 (이름 지정)", ph::TRASH));
                            ui.label(egui::RichText::new("특정 모듈을 서버 전체 또는 한 계정의 모든 사이트에서 제거 (modules/<이름>/ + 캐시 정리). 계정을 비우면 서버 전체입니다.").weak());
                            ui.add_space(4.0);
                            // 지원종료 모듈 안내
                            ui.group(|ui| {
                                ui.label(egui::RichText::new(format!("{}  지원종료(삭제 권장) 모듈", ph::WARNING))
                                    .strong().color(egui::Color32::from_rgb(200, 140, 50)));
                                ui.label(egui::RichText::new("아래 모듈은 최신 Rhymix에서 지원이 종료되어 오류를 유발할 수 있습니다. 클릭하면 이름이 채워집니다.").weak());
                                ui.horizontal_wrapped(|ui| {
                                    for m in DEPRECATED_MODULES {
                                        if ui.button(format!("{}  {m}", ph::WARNING)).clicked() {
                                            self.mod_del_name = (*m).to_string();
                                        }
                                    }
                                });
                            });
                            ui.add_space(6.0);
                            egui::Grid::new("settings_moddel_grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                                grid_label(ui, "모듈 이름");
                                ui.add(egui::TextEdit::singleline(&mut self.mod_del_name).hint_text("예: trackback (영숫자/._- 만)").desired_width(260.0).margin(FIELD_MARGIN));
                                ui.end_row();
                                grid_label(ui, "계정");
                                ui.add(egui::TextEdit::singleline(&mut self.mod_del_acct).hint_text("비우면 서버 전체 / 입력 시 해당 계정만").desired_width(260.0).margin(FIELD_MARGIN));
                                ui.end_row();
                            });
                            if is_deprecated_module(&self.mod_del_name) {
                                ui.label(egui::RichText::new(format!("⚠ '{}' 은(는) 지원종료 모듈입니다 — 삭제 권장.", self.mod_del_name.trim()))
                                    .color(egui::Color32::from_rgb(200, 140, 50)));
                            }
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                let enabled = !running && !self.mod_del_name.trim().is_empty();
                                if ui.add_enabled(enabled, egui::Button::new(format!("{}  모듈 삭제", ph::TRASH)))
                                    .on_hover_text("확인 후 실행 (rm -rf modules/<이름> + 캐시 정리)").clicked() {
                                    do_module_delete = Some(true);
                                }
                                if ui.add_enabled(!self.mod_del_name.trim().is_empty(), egui::Button::new(format!("{}  명령어 보기", ph::FILE_TEXT))).clicked() {
                                    do_module_delete = Some(false);
                                }
                            });
                            ui.add_space(4.0);
                            ui.label(egui::RichText::new("계정별로 설치된 모듈을 보고 선택 삭제하려면: 좌측 고객 옆 🧩 버튼(계정 모듈).").weak());
                        });
                    }
                    SettingsTab::Disk => {
                        card(ui, |ui| {
                            ui.strong(format!("{}  디스크 건강 진단", ph::HARD_DRIVES));
                            ui.label(egui::RichText::new("백업/데이터 디스크의 손상 신호를 사전에 감지합니다. dmesg I/O 에러·ext4 누적 에러카운트·SMART·RO 재마운트·용량을 한 번에 수집(읽기 전용).").weak());
                            ui.label(egui::RichText::new("※ '서버 SSH' 설정 필요. 결과는 아래 로그창에 표시됩니다.").weak());
                            ui.add_space(6.0);
                            if ui.add_enabled(!running, btn_primary(format!("{}  디스크 건강 진단", ph::STETHOSCOPE)))
                                .clicked() { do_disk_health = true; }
                        });
                        ui.add_space(8.0);
                        card(ui, |ui| {
                            ui.strong(format!("{}  무결성 검사 (write → read-back)", ph::SEAL_CHECK));
                            ui.label(egui::RichText::new("SMART가 PASSED여도 못 잡는 silent corruption 확정용. 지정 경로에 임시파일을 쓰고 캐시를 비운 뒤 다시 읽어 해시를 비교합니다. 갓 쓴 데이터가 바뀌면 디스크 교체 신호.").weak());
                            ui.label(egui::RichText::new("⚠ 임시파일 쓰기가 발생합니다(검사 후 자동 삭제). 부하 시 장치가 탈락하면 타임아웃으로 잡힙니다.").weak().color(egui::Color32::from_rgb(200, 140, 50)));
                            ui.add_space(6.0);
                            egui::Grid::new("disk_scrub_grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                                grid_label(ui, "검사 경로");
                                ui.add(egui::TextEdit::singleline(&mut self.disk_path).hint_text("예: /backup").desired_width(260.0).margin(FIELD_MARGIN));
                                ui.end_row();
                            });
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                if ui.add_enabled(!running, egui::Button::new(format!("{}  무결성 검사 (512MB)", ph::SEAL_CHECK)))
                                    .on_hover_text("빠른 확인용 512MB")
                                    .clicked() { do_disk_scrub = Some(512); }
                                if ui.add_enabled(!running, egui::Button::new(format!("{}  정밀 검사 (4GB)", ph::SEAL_CHECK)))
                                    .on_hover_text("간헐적 손상까지 — 시간이 더 걸림")
                                    .clicked() { do_disk_scrub = Some(4096); }
                            });
                        });
                        ui.add_space(8.0);
                        card(ui, |ui| {
                            ui.strong(format!("{}  자동 감시 (사전 경보)", ph::BELL_RINGING));
                            ui.label(egui::RichText::new("서버에 cron 감시를 설치합니다: smartd + 매일 FS에러카운트/dmesg/SMART/용량 점검 + 매주 무결성 스크럽(위 경로). 이상 시 이메일 경보.").weak());
                            ui.add_space(6.0);
                            egui::Grid::new("disk_monitor_grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                                grid_label(ui, "알림 이메일");
                                ui.add(egui::TextEdit::singleline(&mut self.disk_alert_email).hint_text("eond@eond.com").desired_width(260.0).margin(FIELD_MARGIN));
                                ui.end_row();
                            });
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                if ui.add_enabled(!running, btn_primary(format!("{}  자동 감시 설치", ph::BELL_RINGING)))
                                    .on_hover_text("확인 후 서버에 cron/smartd 설치")
                                    .clicked() { do_disk_monitor = Some(true); }
                                if ui.add_enabled(!running, egui::Button::new(format!("{}  감시 제거", ph::BELL_SLASH)))
                                    .clicked() { do_disk_monitor = Some(false); }
                            });
                            ui.label(egui::RichText::new("설치 마지막에 위 주소로 테스트 메일을 자동 발송하고, 사용된 전송수단(mail/sendmail)을 로그에 표시합니다. 메일이 안 오면 서버 MTA(exim/postfix) 큐·스팸함을 점검하세요.").weak());
                        });
                    }
                    SettingsTab::Backup => {
                        card(ui, |ui| {
                            ui.strong(format!("{}  백업 (암호화)", ph::FLOPPY_DISK));
                            ui.label(egui::RichText::new("모든 데이터(고객·도메인·설정·스캔 캐시)를 마스터 비밀번호로 암호화한 .hmbak 파일로 저장합니다.").weak());
                            ui.add_space(4.0);
                            if ui.add(btn_primary(format!("{}  백업 파일 생성", ph::DOWNLOAD_SIMPLE))).clicked() { do_backup = true; }
                            ui.label(egui::RichText::new(format!("저장 폴더: {}", store::backups_root().display())).weak());
                        });
                        ui.add_space(8.0);
                        card(ui, |ui| {
                            ui.strong(format!("{}  복원", ph::ARROW_SQUARE_OUT));
                            ui.colored_label(egui::Color32::from_rgb(220, 120, 120), "주의: 복원하면 현재 모든 데이터가 선택한 백업으로 교체됩니다.");
                            ui.checkbox(&mut self.restore_armed, "현재 데이터를 덮어쓰는 것에 동의");
                            ui.add_space(4.0);
                            ui.label(egui::RichText::new("백업 목록 (최신순):").weak());
                            let backups = store::list_backups();
                            if backups.is_empty() {
                                ui.label(egui::RichText::new("백업 파일이 없습니다.").weak());
                            } else {
                                egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                                    for (path, name) in &backups {
                                        ui.horizontal(|ui| {
                                            if ui.add_enabled(self.restore_armed, egui::Button::new(format!("{}  복원", ph::ARROW_SQUARE_OUT))).clicked() {
                                                do_restore = Some(path.clone());
                                            }
                                            ui.label(egui::RichText::new(name).monospace());
                                        });
                                    }
                                });
                            }
                            ui.add_space(6.0);
                            ui.label(egui::RichText::new("다른 경로의 백업 파일 직접 지정:").weak());
                            ui.horizontal(|ui| {
                                ui.add(egui::TextEdit::singleline(&mut self.restore_path).hint_text("/경로/hostmover-backup-….hmbak").desired_width(360.0).margin(FIELD_MARGIN));
                                if ui.add_enabled(self.restore_armed && !self.restore_path.trim().is_empty(), egui::Button::new("이 경로 복원")).clicked() {
                                    do_restore = Some(std::path::PathBuf::from(self.restore_path.trim()));
                                }
                            });
                        });
                    }
                }
            });
        });
        self.show_pw = show_pw;
        if close { self.view = MainView::Domain; }
        if do_backup {
            match store::export_backup(&self.master_pw, &self.store) {
                Ok(path) => {
                    self.last_ok = Some(true);
                    self.status = format!("백업 생성: {}", path.display());
                    self.log.push(format!("백업 파일 생성: {}", path.display()));
                }
                Err(e) => {
                    self.last_ok = Some(false);
                    self.status = format!("백업 실패: {e}");
                    self.log.push(format!("백업 실패: {e}"));
                }
            }
        }
        if let Some(path) = do_restore {
            match store::import_backup(&self.master_pw, &path) {
                Ok(s) => {
                    self.store = s;
                    self.dirty = true;
                    self.save();
                    self.hydrate_scan_cache();
                    self.restore_armed = false;
                    self.last_ok = Some(true);
                    self.status = format!("복원 완료: {}", path.display());
                    self.log.push(format!("백업 복원: {}", path.display()));
                }
                Err(e) => {
                    self.last_ok = Some(false);
                    self.status = format!("복원 실패: {e}");
                    self.log.push(format!("복원 실패: {e}"));
                }
            }
        }
        if do_test {
            self.hestia_test();
        }
        if do_import_users {
            self.hestia_import_users();
        }
        if do_panel_probe {
            match ops::hestia_panel_probe(&self.store.settings) {
                Ok(report) => {
                    for l in report.lines() { self.log.push(l.to_string()); }
                    self.cmd_view = Some(CmdView { title: "패널 네트워크 진단".into(), command: report });
                    self.status = "패널 네트워크 진단 완료".into();
                }
                Err(e) => {
                    self.last_ok = Some(false);
                    self.status = format!("진단 실패: {e}");
                    self.log.push(format!("패널 네트워크 진단 실패: {e}"));
                }
            }
        }
        if do_panel_diag {
            match ops::build_panel_diagnose(&self.store.settings) {
                Ok(job) => {
                    self.running = true;
                    self.last_ok = None;
                    self.status = "패널 서버 진단 중...".into();
                    let ctx2 = ctx.clone();
                    ops::spawn(job, self.tx.clone(), move || ctx2.request_repaint());
                }
                Err(e) => {
                    self.last_ok = Some(false);
                    self.status = format!("진단 실패: {e}");
                    self.log.push(format!("패널 서버 진단: {e}"));
                }
            }
        }
        if do_panel_recover {
            match ops::build_panel_recover(&self.store.settings) {
                Ok(job) => { self.eond_confirm = Some(job); }
                Err(e) => {
                    self.last_ok = Some(false);
                    self.status = format!("패널 복구 준비 실패: {e}");
                    self.log.push(format!("패널 복구: {e}"));
                }
            }
        }
        if do_disk_health {
            match ops::build_disk_health(&self.store.settings) {
                Ok(job) => {
                    self.running = true;
                    self.last_ok = None;
                    self.status = "디스크 건강 진단 중...".into();
                    let ctx2 = ctx.clone();
                    ops::spawn(job, self.tx.clone(), move || ctx2.request_repaint());
                }
                Err(e) => {
                    self.last_ok = Some(false);
                    self.status = format!("디스크 진단 실패: {e}");
                    self.log.push(format!("디스크 건강 진단: {e}"));
                }
            }
        }
        if let Some(size_mb) = do_disk_scrub {
            let path = self.disk_path.clone();
            match ops::build_disk_scrub(&self.store.settings, &path, size_mb) {
                Ok(job) => {
                    self.running = true;
                    self.last_ok = None;
                    self.status = "디스크 무결성 검사 중...".into();
                    let ctx2 = ctx.clone();
                    ops::spawn(job, self.tx.clone(), move || ctx2.request_repaint());
                }
                Err(e) => {
                    self.last_ok = Some(false);
                    self.status = format!("무결성 검사 실패: {e}");
                    self.log.push(format!("디스크 무결성 검사: {e}"));
                }
            }
        }
        if let Some(install) = do_disk_monitor {
            let built = if install {
                ops::build_disk_monitor_install(&self.store.settings, &self.disk_alert_email, &self.disk_path)
            } else {
                ops::build_disk_monitor_uninstall(&self.store.settings)
            };
            match built {
                Ok(job) => { self.eond_confirm = Some(job); }
                Err(e) => {
                    self.last_ok = Some(false);
                    self.status = format!("자동 감시 준비 실패: {e}");
                    self.log.push(format!("자동 감시: {e}"));
                }
            }
        }
        if let Some(run) = do_bulk_update {
            match ops::build_bulk_git_update(&self.store.settings) {
                Ok(job) => {
                    if run {
                        self.eond_confirm = Some(job);
                    } else {
                        self.cmd_view = Some(CmdView { title: job.title.clone(), command: render_command(&job, self.show_pw) });
                    }
                }
                Err(e) => {
                    self.log.push(format!("일괄 업데이트: {e}"));
                    self.status = format!("오류: {e}");
                    self.last_ok = Some(false);
                }
            }
        }
        if let Some(run) = do_module_delete {
            let (name, acct) = (self.mod_del_name.clone(), self.mod_del_acct.clone());
            match ops::build_module_delete(&self.store.settings, &name, &acct) {
                Ok(job) => {
                    if run {
                        self.eond_confirm = Some(job);
                    } else {
                        self.cmd_view = Some(CmdView { title: job.title.clone(), command: render_command(&job, self.show_pw) });
                    }
                }
                Err(e) => {
                    self.log.push(format!("모듈 삭제: {e}"));
                    self.status = format!("오류: {e}");
                    self.last_ok = Some(false);
                }
            }
        }
    }

    /// 계정 모듈 목록을 백그라운드로 조회 (UI 안 멈춤)
    fn load_account_modules(&mut self, ci: usize, ctx: &egui::Context) {
        if self.scanning { return; }
        let acct = match self.store.customers.get(ci) {
            Some(c) => c.name.clone(),
            None => return,
        };
        self.acct_mods_for = Some(ci);
        self.acct_mods.clear();
        let dom = self.acct_mod_domain.trim().to_string();
        let label = if dom.is_empty() { format!("{acct} 전체") } else { dom.clone() };
        self.scanning = true;
        self.acct_mods_status = format!("{label}: 모듈 조회 중...");
        let (tx, settings, ctx) = (self.scan_tx.clone(), self.store.settings.clone(), ctx.clone());
        std::thread::spawn(move || {
            let scope = if dom.is_empty() { None } else { Some(dom.as_str()) };
            let res = ops::list_account_modules(&settings, &acct, scope);
            let _ = tx.send(ScanMsg::AccountModules { ci, label, res });
            ctx.request_repaint();
        });
    }

    /// WordPress 플러그인 버전비교 스캔 (백그라운드). 대상 = 선택 도메인의 현재/신규 서버.
    fn scan_wp_plugins(&mut self, ci: usize, di: usize, ctx: &egui::Context) {
        if self.wp_scanning { return; }
        let (server, mut c, domain_id, dn) = {
            let dom = &self.store.customers[ci].domains[di];
            let c = dom.cms_install.clone();
            let server = if c.use_asis { dom.asis.clone() } else { dom.tobe.clone() };
            (server, c, dom.id, dom.name.clone())
        };
        c.hestia_user = server.ftp_id.clone();
        c.hestia_pass = server.ftp_pw.clone();
        if c.hestia_user.trim().is_empty() { c.hestia_user = self.store.customers[ci].name.clone(); }
        let src = self.store.settings.wp_source_local.clone();
        let use_root = self.use_root;
        let kind = self.wp_kind;
        self.wp_scanning = true;
        self.wp_scan_status = format!("{dn}: {} 버전 비교 중...", kind.label());
        let (tx, ctx2) = (self.scan_tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let res = ops::wp_asset_scan(&server, &c, &src, &dn, use_root, kind);
            let _ = tx.send(ScanMsg::WpPlugins { domain_id, kind, res });
            ctx2.request_repaint();
        });
    }

    /// 🧩 계정 관리 페이지 — 사이트(공유 캐시 필터) / 모듈(선택 삭제)
    fn account_modules_page(&mut self, ctx: &egui::Context, ci: usize) {
        let acct = match self.store.customers.get(ci) {
            Some(c) => c.name.clone(),
            None => { self.view = MainView::Domain; return; }
        };
        let dom_names: Vec<String> = self.store.customers.get(ci)
            .map(|c| c.domains.iter().filter(|d| d.deleted_at.is_none()).map(|d| d.name.clone()).collect())
            .unwrap_or_default();
        let running = self.running;
        let scanning = self.scanning;
        let orange = egui::Color32::from_rgb(200, 140, 50);
        let hcol = egui::Color32::from_gray(210);
        let mut close = false;
        let mut do_scan = false;
        let mut do_load_mods = false;
        let mut do_delete: Option<bool> = None;
        let mut do_update: Option<bool> = None; // 실행/명령어보기 (선택만)
        let mut do_dns = false;
        let mut do_scan_sel = false;
        let mut do_backup_sel = false;
        let mut alias_loads: Vec<(String, String)> = Vec::new();
        let mut sel_all_sites: Option<bool> = None;
        let mut select_all = None;
        let mut select_deprecated = false;
        let has_rows = self.all_sites.iter().any(|r| r.account == acct);
        let frame = egui::Frame::central_panel(&ctx.style()).inner_margin(egui::Margin::symmetric(14, 12));
        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading(format!("{}  계정 관리 — {acct}", ph::PUZZLE_PIECE));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(format!("{}  도메인으로", ph::X)).clicked() { close = true; }
                });
            });
            ui.label(egui::RichText::new("사이트 데이터는 전체 사이트 스캔 캐시를 공유합니다. (설정 > 서버 SSH 필요)").weak());
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                for (t, label) in [(AcctTab::Sites, "  사이트 (CMS·버전·업데이트)  "), (AcctTab::Modules, "  모듈 (선택 삭제)  ")] {
                    if ui.selectable_label(self.acct_tab == t, label).clicked() { self.acct_tab = t; }
                }
            });
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(8.0);
            match self.acct_tab {
                AcctTab::Sites => {
                    ui.horizontal(|ui| {
                        if ui.add_enabled(!running && !scanning, btn_primary(format!("{}  이 계정 스캔", ph::ARROWS_CLOCKWISE))).on_hover_text("이 계정만 다시 스캔해 공유 캐시 갱신").clicked() { do_scan = true; }
                        if scanning { ui.spinner(); ui.label("스캔 중…"); }
                        if has_rows {
                            if ui.button("전체 선택").clicked() { sel_all_sites = Some(true); }
                            if ui.button("전체 해제").clicked() { sel_all_sites = Some(false); }
                        }
                    });
                    if !self.all_sites_status.is_empty() { ui.label(egui::RichText::new(&self.all_sites_status).weak()); }
                    ui.add_space(6.0);
                    if !has_rows {
                        ui.label(egui::RichText::new("이 계정의 스캔 데이터가 없습니다. ‘이 계정 스캔’ 또는 전체 사이트에서 전체 스캔하세요.").weak());
                    } else {
                        let avail = ui.available_height();
                        let (sa, tl) = self.render_sites_table(ui, false, &acct, "", (avail - 96.0).max(140.0));
                        if let Some(v) = sa { sel_all_sites = Some(v); }
                        alias_loads = tl;
                        ui.add_space(6.0);
                        ui.separator();
                        ui.add_space(4.0);
                        let seln = self.all_sites.iter().filter(|r| r.account == acct && r.sel).count();
                        ui.horizontal_wrapped(|ui| {
                            if ui.add_enabled(!running && seln > 0, btn_primary(format!("{}  선택 {seln}개 업데이트", ph::ARROWS_CLOCKWISE))).clicked() { do_update = Some(true); }
                            if ui.add_enabled(!scanning && seln > 0, egui::Button::new(format!("{}  선택 {seln}개만 스캔", ph::ARROWS_CLOCKWISE))).clicked() { do_scan_sel = true; }
                            if ui.add_enabled(!running && seln > 0, egui::Button::new(format!("{}  선택 파일·DB 백업", ph::DOWNLOAD_SIMPLE))).on_hover_text("선택 사이트의 파일+DB를 로컬로 내려받기").clicked() { do_backup_sel = true; }
                            if ui.add_enabled(seln > 0, egui::Button::new(format!("{}  DNS 체크(A)", ph::GLOBE_HEMISPHERE_WEST))).clicked() { do_dns = true; }
                            if ui.add_enabled(seln > 0, egui::Button::new(format!("{}  명령어 보기", ph::FILE_TEXT))).clicked() { do_update = Some(false); }
                        });
                        ui.label(egui::RichText::new("업데이트는 CMS 유형 자동 분기(WordPress=wp-cli, Rhymix/그누보드=git/오버레이). 상태=최신 비교, DNS 체크로 A 레코드 표시.").weak());
                    }
                }
                AcctTab::Modules => {
                    ui.horizontal(|ui| {
                        ui.label("도메인:");
                        let cur = if self.acct_mod_domain.trim().is_empty() { "전체 (계정)".to_string() } else { self.acct_mod_domain.clone() };
                        egui::ComboBox::from_id_salt("acct_mod_dom").selected_text(cur).show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.acct_mod_domain, String::new(), "전체 (계정)");
                            for d in &dom_names {
                                ui.selectable_value(&mut self.acct_mod_domain, d.clone(), d.as_str());
                            }
                        });
                        if ui.add_enabled(!running && !scanning, btn_primary(format!("{}  모듈 불러오기", ph::ARROWS_CLOCKWISE))).clicked() { do_load_mods = true; }
                        if scanning { ui.spinner(); }
                        if !self.acct_mods.is_empty() {
                            if ui.button("전체 선택").clicked() { select_all = Some(true); }
                            if ui.button("전체 해제").clicked() { select_all = Some(false); }
                            if ui.button(format!("{}  지원종료만 선택", ph::WARNING)).clicked() { select_deprecated = true; }
                        }
                    });
                    if !self.acct_mods_status.is_empty() { ui.label(egui::RichText::new(&self.acct_mods_status).weak()); }
                    ui.add_space(6.0);
                    if self.acct_mods.is_empty() {
                        ui.label(egui::RichText::new("‘모듈 불러오기’를 눌러 설치 모듈을 조회하세요. (행 클릭으로 선택)").weak());
                    } else {
                        let avail = ui.available_height();
                        egui::ScrollArea::vertical().auto_shrink([false, false]).max_height((avail - 70.0).max(140.0)).show(ui, |ui| {
                            for (name, doms, sel) in &mut self.acct_mods {
                                let dep = is_deprecated_module(name);
                                let namecol = if dep { orange } else { hcol };
                                let cols: SiteCols = vec![
                                    (name.clone(), namecol, 200.0),
                                    (if dep { "지원종료".to_string() } else { String::new() }, orange, 80.0),
                                    (format!("{}개 사이트: {}", doms.len(), doms.join(", ")), egui::Color32::from_gray(150), 480.0),
                                ];
                                if site_row(ui, *sel, &cols) { *sel = !*sel; }
                            }
                        });
                        ui.add_space(6.0);
                        ui.separator();
                        ui.add_space(4.0);
                        let seln = self.acct_mods.iter().filter(|(_, _, s)| *s).count();
                        ui.horizontal(|ui| {
                            if ui.add_enabled(!running && seln > 0, egui::Button::new(format!("{}  선택 {seln}개 삭제", ph::TRASH)))
                                .on_hover_text("확인 후 실행 (선택 모듈을 rm -rf + 캐시 정리)").clicked() {
                                do_delete = Some(true);
                            }
                            if ui.add_enabled(seln > 0, egui::Button::new(format!("{}  명령어 보기", ph::FILE_TEXT))).clicked() { do_delete = Some(false); }
                        });
                    }
                }
            }
        });
        if let Some(v) = sel_all_sites { for r in self.all_sites.iter_mut().filter(|r| r.account == acct) { r.sel = v; } }
        if let Some(v) = select_all { for m in &mut self.acct_mods { m.2 = v; } }
        if select_deprecated { for m in &mut self.acct_mods { m.2 = is_deprecated_module(&m.0); } }
        if close { self.view = MainView::Domain; }
        self.load_aliases_batch(alias_loads, ctx);
        if do_scan { self.rescan_account_into_all(acct.clone(), ctx); }
        if do_load_mods { self.load_account_modules(ci, ctx); }
        if do_scan_sel {
            let pairs: Vec<(String, String)> = self.all_sites.iter().filter(|r| r.account == acct && r.sel).map(|r| (r.account.clone(), r.domain.clone())).collect();
            self.rescan_selected(pairs, ctx);
        }
        if do_backup_sel {
            let pairs: Vec<(String, String)> = self.all_sites.iter().filter(|r| r.account == acct && r.sel).map(|r| (r.account.clone(), r.domain.clone())).collect();
            self.backup_sites(pairs);
        }
        if do_dns {
            let doms: Vec<String> = self.all_sites.iter().filter(|r| r.account == acct && r.sel).map(|r| r.domain.clone()).collect();
            self.dns_check(doms, ctx);
        }
        if let Some(run) = do_update {
            let domains: Vec<String> = self.all_sites.iter().filter(|r| r.account == acct && r.sel).map(|r| r.domain.clone()).collect();
            match ops::build_account_git_update(&self.store.settings, &acct, &domains) {
                Ok(job) => {
                    if run {
                        self.pending_rescan = domains.iter().map(|d| (acct.clone(), d.clone())).collect();
                        self.eond_confirm = Some(job);
                    }
                    else { self.cmd_view = Some(CmdView { title: job.title.clone(), command: render_command(&job, self.show_pw) }); }
                }
                Err(e) => {
                    self.log.push(format!("계정 업데이트: {e}"));
                    self.status = format!("오류: {e}");
                    self.last_ok = Some(false);
                }
            }
        }
        if let Some(run) = do_delete {
            let selected: Vec<String> = self.acct_mods.iter().filter(|(_, _, s)| *s).map(|(m, _, _)| m.clone()).collect();
            let dom = self.acct_mod_domain.trim().to_string();
            let scope = if dom.is_empty() { None } else { Some(dom.as_str()) };
            match ops::build_account_modules_delete(&self.store.settings, &acct, scope, &selected) {
                Ok(job) => {
                    if run { self.eond_confirm = Some(job); }
                    else { self.cmd_view = Some(CmdView { title: job.title.clone(), command: render_command(&job, self.show_pw) }); }
                }
                Err(e) => {
                    self.log.push(format!("계정 모듈 삭제: {e}"));
                    self.status = format!("오류: {e}");
                    self.last_ok = Some(false);
                }
            }
        }
    }

    /// 저장된 스캔 캐시를 all_sites 로 적재 (앱 시작 시)
    fn hydrate_scan_cache(&mut self) {
        if self.store.scan_cache.is_empty() { return; }
        self.all_sites = self.store.scan_cache.iter()
            .map(|c| AcctSiteRow::new(c.account.clone(), c.domain.clone(), c.kind.clone(), c.version.clone(), c.status.clone(), c.git, c.file_bytes, c.db_bytes))
            .collect();
        self.all_sites_status = format!("캐시 {}개 ({}) — 최신화하려면 전체 스캔", self.all_sites.len(), ago_text(self.store.scan_cache_at));
    }

    /// all_sites → 캐시 저장
    fn save_scan_cache(&mut self) {
        self.store.scan_cache = self.all_sites.iter().map(|r| CachedSite {
            account: r.account.clone(), domain: r.domain.clone(), kind: r.kind.clone(),
            version: r.version.clone(), status: r.status.clone(), git: r.git, file_bytes: r.file_bytes, db_bytes: r.db_bytes,
        }).collect();
        self.store.scan_cache_at = now_unix();
        self.dirty = true;
        self.save();
    }

    /// 전체 사이트 스캔을 백그라운드로 (UI 안 멈춤)
    fn load_all_sites(&mut self, ctx: &egui::Context) {
        if self.scanning { return; }
        self.scanning = true;
        self.all_sites_status = "전체 스캔 중...".into();
        let (tx, settings, ctx) = (self.scan_tx.clone(), self.store.settings.clone(), ctx.clone());
        std::thread::spawn(move || {
            let res = ops::scan_all_sites(&settings).map(|rows| {
                let (rx, wp, gn) = ops::latest_versions();
                rows.into_iter().map(|(a, d, k, v, sst, g, fb, db)| {
                    let local = compute_status(&k, &v, g, &rx, &wp, &gn);
                    let st = if local == "-" && !sst.is_empty() && sst != "-" { sst } else { local };
                    (a, d, k, v, st, g, fb, db)
                }).collect()
            });
            let _ = tx.send(ScanMsg::AllSites { res });
            ctx.request_repaint();
        });
    }

    /// 전체 리스트에서 특정 계정만 재스캔 (백그라운드)
    fn rescan_account_into_all(&mut self, account: String, ctx: &egui::Context) {
        if self.scanning || account.trim().is_empty() { return; }
        self.scanning = true;
        self.all_sites_status = format!("계정 {account} 재스캔 중...");
        let (tx, settings, ctx) = (self.scan_tx.clone(), self.store.settings.clone(), ctx.clone());
        std::thread::spawn(move || {
            let res = ops::scan_account_sites(&settings, &account).map(|rows| {
                let (rx, wp, gn) = ops::latest_versions();
                rows.into_iter().map(|(d, k, v, sst, g, fb, db)| {
                    let local = compute_status(&k, &v, g, &rx, &wp, &gn);
                    let st = if local == "-" && !sst.is_empty() && sst != "-" { sst } else { local };
                    (d, k, v, st, g, fb, db)
                }).collect()
            });
            let _ = tx.send(ScanMsg::AccountIntoAll { account, res });
            ctx.request_repaint();
        });
    }

    /// 업데이트 직후 선택 사이트만 재스캔해 상태/버전 갱신 (백그라운드)
    fn rescan_selected(&mut self, pairs: Vec<(String, String)>, ctx: &egui::Context) {
        if pairs.is_empty() || self.scanning { return; }
        self.scanning = true;
        self.all_sites_status = format!("업데이트 후 {}개 사이트 재스캔 중...", pairs.len());
        let (tx, settings, ctx) = (self.scan_tx.clone(), self.store.settings.clone(), ctx.clone());
        std::thread::spawn(move || {
            let res = ops::scan_selected_sites(&settings, &pairs).map(|rows| {
                let (rx, wp, gn) = ops::latest_versions();
                rows.into_iter().map(|(a, d, k, v, g, fb, db)| {
                    let st = compute_status(&k, &v, g, &rx, &wp, &gn);
                    (a, d, k, v, st, g, fb, db)
                }).collect()
            });
            let _ = tx.send(ScanMsg::SelectedSites { res });
            ctx.request_repaint();
        });
    }

    /// 보이는 도메인들의 alias 를 한 백그라운드 스레드에서 순차 조회
    fn load_aliases_batch(&mut self, pairs: Vec<(String, String)>, ctx: &egui::Context) {
        if pairs.is_empty() { return; }
        let (tx, settings, ctx) = (self.scan_tx.clone(), self.store.settings.clone(), ctx.clone());
        std::thread::spawn(move || {
            let results: Vec<(String, String, Vec<String>)> = pairs.into_iter()
                .map(|(a, d)| {
                    let al = ops::list_aliases(&settings, &a, &d).unwrap_or_default();
                    (a, d, al)
                })
                .collect();
            let _ = tx.send(ScanMsg::AliasesBatch { results });
            ctx.request_repaint();
        });
    }

    /// 선택 사이트의 파일+DB를 로컬로 백업 (확인 모달 경유)
    fn backup_sites(&mut self, pairs: Vec<(String, String)>) {
        if pairs.is_empty() { return; }
        let dest = store::backups_root().join("sites");
        let dest_s = dest.to_string_lossy().to_string();
        match ops::build_local_backup(&self.store.settings, &pairs, &dest_s) {
            Ok(job) => { self.eond_confirm = Some(job); }
            Err(e) => {
                self.log.push(format!("사이트 백업: {e}"));
                self.status = format!("오류: {e}");
                self.last_ok = Some(false);
            }
        }
    }

    /// 선택 도메인들의 DNS A 레코드 조회 (백그라운드, 로컬 dig)
    fn dns_check(&mut self, domains: Vec<String>, ctx: &egui::Context) {
        if domains.is_empty() { return; }
        self.status = format!("DNS 조회 중... ({}개)", domains.len());
        let (tx, ctx) = (self.scan_tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let results: Vec<(String, String)> = domains.into_iter().map(|d| {
                let a = ops::resolve_a(&d);
                (d, a)
            }).collect();
            let _ = tx.send(ScanMsg::Dns { results });
            ctx.request_repaint();
        });
    }

    /// 백그라운드 스캔 결과 수신 → 해당 목록 채움
    fn drain_scans(&mut self) {
        while let Ok(msg) = self.scan_rx.try_recv() {
            self.scanning = false;
            match msg {
                ScanMsg::AllSites { res } => match res {
                    Ok(list) => {
                        self.all_sites = list.into_iter().map(|(account, domain, kind, version, status, git, file_bytes, db_bytes)| AcctSiteRow::new(account, domain, kind, version, status, git, file_bytes, db_bytes)).collect();
                        self.all_sites_status = format!("전체 사이트 {}개 스캔됨", self.all_sites.len());
                        self.save_scan_cache();
                        self.last_ok = Some(true);
                        self.status = self.all_sites_status.clone();
                    }
                    Err(e) => {
                        self.all_sites_status = format!("스캔 실패: {e}");
                        self.last_ok = Some(false);
                        self.status = format!("전체 스캔 실패: {e}");
                        self.log.push(format!("전체 사이트 스캔 실패: {e}"));
                    }
                },
                ScanMsg::AccountIntoAll { account, res } => match res {
                    Ok(list) => {
                        // 해당 계정 행 제거 후 새 결과 병합
                        self.all_sites.retain(|r| r.account != account);
                        for (domain, kind, version, status, git, file_bytes, db_bytes) in list {
                            self.all_sites.push(AcctSiteRow::new(account.clone(), domain, kind, version, status, git, file_bytes, db_bytes));
                        }
                        self.all_sites.sort_by(|a, b| (a.account.as_str(), a.domain.as_str()).cmp(&(b.account.as_str(), b.domain.as_str())));
                        self.all_sites_status = format!("계정 {account} 재스캔 완료 — 전체 {}개", self.all_sites.len());
                        self.save_scan_cache();
                        self.last_ok = Some(true);
                        self.status = self.all_sites_status.clone();
                    }
                    Err(e) => {
                        self.all_sites_status = format!("계정 재스캔 실패: {e}");
                        self.last_ok = Some(false);
                        self.status = format!("계정 재스캔 실패: {e}");
                    }
                },
                ScanMsg::SelectedSites { res } => match res {
                    Ok(list) => {
                        let n = list.len();
                        for (account, domain, kind, version, status, git, file_bytes, db_bytes) in list {
                            if let Some(r) = self.all_sites.iter_mut().find(|r| r.account == account && r.domain == domain) {
                                r.kind = kind; r.version = version; r.status = status; r.git = git; r.file_bytes = file_bytes; r.db_bytes = db_bytes;
                            } else {
                                self.all_sites.push(AcctSiteRow::new(account, domain, kind, version, status, git, file_bytes, db_bytes));
                            }
                        }
                        self.save_scan_cache();
                        self.all_sites_status = format!("업데이트 후 {n}개 사이트 갱신됨");
                        self.status = self.all_sites_status.clone();
                        self.last_ok = Some(true);
                    }
                    Err(e) => {
                        self.all_sites_status = format!("업데이트 후 재스캔 실패: {e}");
                        self.log.push(format!("업데이트 후 재스캔 실패: {e}"));
                    }
                },
                ScanMsg::Dns { results } => {
                    for (dom, a) in results {
                        for r in self.all_sites.iter_mut().filter(|r| r.domain == dom) { r.a_record = a.clone(); }
                    }
                    self.all_sites_status = "DNS 조회 완료".into();
                    self.status = "DNS 조회 완료".into();
                },
                ScanMsg::AliasesBatch { results } => {
                    for (account, domain, aliases) in results {
                        for r in self.all_sites.iter_mut().filter(|r| r.account == account && r.domain == domain) {
                            r.aliases = Some(aliases.clone());
                        }
                    }
                },
                ScanMsg::AccountModules { ci, label, res } => match res {
                    Ok(list) => {
                        self.acct_mods_for = Some(ci);
                        self.acct_mods = list.into_iter().map(|(m, doms)| (m, doms, false)).collect();
                        self.acct_mods_status = format!("{label}: 모듈 {}종 로드됨", self.acct_mods.len());
                        self.last_ok = Some(true);
                        self.status = self.acct_mods_status.clone();
                    }
                    Err(e) => {
                        self.acct_mods_status = format!("로드 실패: {e}");
                        self.last_ok = Some(false);
                        self.status = format!("모듈 로드 실패: {e}");
                        self.log.push(format!("계정 모듈 로드 실패: {e}"));
                    }
                },
                ScanMsg::WpPlugins { domain_id, kind, res } => {
                    self.wp_scanning = false;
                    match res {
                        Ok(rows) => {
                            // 결과가 현재 보고 있는 종류와 같을 때만 선택을 자동 갱신(다른 종류는 캐시만 채움)
                            if kind == self.wp_kind {
                                self.wp_sel.clear();
                                // 기본 선택: 업데이트 가능 + 신규(로컬전용)
                                for r in &rows {
                                    if matches!(r.diff, ops::WpDiff::Update | ops::WpDiff::LocalOnly) {
                                        self.wp_sel.insert(r.slug.clone());
                                    }
                                }
                            }
                            let up = rows.iter().filter(|r| r.diff == ops::WpDiff::Update).count();
                            self.wp_scan_status = format!("{} {}개 · 업데이트 가능 {up}개 (자동선택됨)", kind.label(), rows.len());
                            self.status = self.wp_scan_status.clone();
                            self.last_ok = Some(true);
                            self.wp_cache.insert((domain_id, kind), rows);
                        }
                        Err(e) => {
                            self.wp_cache.remove(&(domain_id, kind));
                            self.wp_scan_status = format!("스캔 실패: {e}");
                            self.status = self.wp_scan_status.clone();
                            self.last_ok = Some(false);
                            self.log.push(format!("WP {} 스캔 실패: {e}", kind.label()));
                        }
                    }
                }
            }
        }
    }

    /// 사이트 표(공용) — all_sites 를 필터(fa=계정, fs=검색)해서 렌더. 행 클릭 선택, 화살표로 alias 펼침.
    /// 반환: (헤더 전체선택 변경, alias 조회 필요한 (계정,도메인) 목록)
    fn render_sites_table(&mut self, ui: &mut egui::Ui, show_account: bool, fa: &str, fs: &str, max_h: f32) -> (Option<bool>, Vec<(String, String)>) {
        let hc = ui.visuals().strong_text_color();
        let tc = ui.visuals().text_color();
        let weak = ui.visuals().weak_text_color();
        let fsl = fs.to_lowercase();
        let vis = |r: &AcctSiteRow| (fa.is_empty() || r.account == fa) && (fsl.is_empty() || r.domain.to_lowercase().contains(&fsl));
        let mut sel_all = None;
        let mut to_load: Vec<(String, String)> = Vec::new();
        let any = self.all_sites.iter().any(|r| vis(r));
        let mut allsel = any && self.all_sites.iter().filter(|r| vis(r)).all(|r| r.sel);
        if ui.checkbox(&mut allsel, "보이는 항목 전체 선택").changed() { sel_all = Some(allsel); }
        ui.add_space(2.0);
        let lead = (if show_account { 110.0 } else { 0.0 }) + 175.0 + 100.0 + 76.0 + 150.0;
        egui::ScrollArea::both().auto_shrink([false, false]).max_height(max_h).show(ui, |ui| {
            let mut header: SiteCols = Vec::new();
            if show_account { header.push(("계정".to_string(), hc, 110.0)); }
            for (h, w) in [("도메인", 175.0), ("CMS", 100.0), ("버전", 76.0), ("상태", 150.0), ("ALIAS", 200.0), ("A레코드", 120.0), ("파일", 60.0), ("DB", 60.0)] {
                header.push((h.to_string(), hc, w));
            }
            site_info_row(ui, &header);
            let (mut tf, mut tdb) = (0u64, 0u64);
            for i in 0..self.all_sites.len() {
                if !vis(&self.all_sites[i]) { continue; }
                let r = &mut self.all_sites[i];
                // 보이는 행은 alias 자동 조회 (한 번만)
                if r.aliases.is_none() && !r.alias_req {
                    r.alias_req = true;
                    to_load.push((r.account.clone(), r.domain.clone()));
                }
                let alias_txt = match &r.aliases {
                    None => "…".to_string(),
                    Some(a) if a.is_empty() => "-".to_string(),
                    Some(a) => a.join(", "),
                };
                let kc = kind_color(&r.kind, r.git);
                let sc = status_color(&r.status);
                let mut cols: SiteCols = Vec::new();
                if show_account { cols.push((r.account.clone(), egui::Color32::GRAY, 110.0)); }
                cols.push((r.domain.clone(), tc, 175.0));
                cols.push((format!("{}{}", r.kind, if r.git { " (git)" } else { "" }), kc, 100.0));
                cols.push((r.version.clone(), tc, 76.0));
                cols.push((if r.status.is_empty() { "-".to_string() } else { r.status.clone() }, sc, 150.0));
                cols.push((alias_txt, weak, 200.0));
                cols.push((if r.a_record.is_empty() { "-".to_string() } else { r.a_record.clone() }, tc, 120.0));
                cols.push((human_bytes(r.file_bytes), tc, 60.0));
                cols.push((human_bytes(r.db_bytes), tc, 60.0));
                if site_row(ui, r.sel, &cols) { r.sel = !r.sel; }
                tf += r.file_bytes;
                tdb += r.db_bytes;
            }
            let tot: SiteCols = vec![("합계".to_string(), hc, lead + 200.0 + 120.0), (human_bytes(tf), hc, 60.0), (human_bytes(tdb), hc, 60.0)];
            site_info_row(ui, &tot);
        });
        (sel_all, to_load)
    }

    /// 📋 전체 사이트 통합 페이지 — 모든 계정의 사이트 CMS/버전/용량 + 멀티선택 일괄 업데이트
    fn all_sites_page(&mut self, ctx: &egui::Context) {
        let running = self.running;
        let scanning = self.scanning;
        let mut do_load = false;
        let mut do_update: Option<(bool, bool)> = None;
        let mut close = false;
        let mut sel_all: Option<bool> = None;
        let mut do_rescan_acct = false;
        let mut do_dns = false;
        let mut do_scan_selected = false;
        let mut do_backup_sites = false;
        let mut alias_loads: Vec<(String, String)> = Vec::new();
        let frame = egui::Frame::central_panel(&ctx.style()).inner_margin(egui::Margin::symmetric(14, 12));
        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading(format!("{}  전체 사이트", ph::LIST_BULLETS));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(format!("{}  도메인으로", ph::X)).clicked() { close = true; }
                });
            });
            ui.label(egui::RichText::new("모든 계정(/home/*/web/*)의 사이트를 스캔합니다. (설정 > 서버 SSH 필요)").weak());
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(!running && !scanning, btn_primary(format!("{}  전체 스캔", ph::ARROWS_CLOCKWISE))).clicked() { do_load = true; }
                if ui.add_enabled(!running && !scanning && !self.all_filter_acct.is_empty(), egui::Button::new(format!("{}  이 계정만 재스캔", ph::ARROWS_CLOCKWISE)))
                    .on_hover_text("계정 필터에 선택된 계정만 다시 스캔해 갱신").clicked() { do_rescan_acct = true; }
                if scanning { ui.spinner(); ui.label("스캔 중…"); }
            });
            // 계정 필터 + 도메인 검색
            if !self.all_sites.is_empty() {
                ui.horizontal(|ui| {
                    ui.label("계정:");
                    let cur = if self.all_filter_acct.is_empty() { "전체".to_string() } else { self.all_filter_acct.clone() };
                    egui::ComboBox::from_id_salt("all_acct_filter").selected_text(cur).show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.all_filter_acct, String::new(), "전체");
                        let mut accts: Vec<String> = self.all_sites.iter().map(|r| r.account.clone()).collect();
                        accts.sort();
                        accts.dedup();
                        for a in accts {
                            ui.selectable_value(&mut self.all_filter_acct, a.clone(), a.as_str());
                        }
                    });
                    ui.add_space(8.0);
                    ui.label("검색:");
                    ui.add(egui::TextEdit::singleline(&mut self.all_search).hint_text("도메인 일부").desired_width(180.0).margin(FIELD_MARGIN));
                    if !self.all_search.is_empty() && ui.button(ph::X).clicked() { self.all_search.clear(); }
                });
            }
            if !self.all_sites_status.is_empty() { ui.label(egui::RichText::new(&self.all_sites_status).weak()); }
            ui.add_space(6.0);
            if self.all_sites.is_empty() {
                ui.label(egui::RichText::new("‘전체 스캔’으로 모든 계정의 사이트를 조회하세요. (사이트가 많으면 다소 시간이 걸립니다)").weak());
            } else {
                let fa = self.all_filter_acct.clone();
                let fs = self.all_search.trim().to_string();
                let fsl = fs.to_lowercase();
                let shown = self.all_sites.iter().filter(|r| (fa.is_empty() || r.account == fa) && (fsl.is_empty() || r.domain.to_lowercase().contains(&fsl))).count();
                let avail = ui.available_height();
                let (sa, tl) = self.render_sites_table(ui, true, &fa, &fs, (avail - 96.0).max(140.0));
                if let Some(v) = sa { sel_all = Some(v); }
                alias_loads = tl;
                ui.add_space(6.0);
                ui.separator();
                ui.add_space(4.0);
                let seln = self.all_sites.iter().filter(|r| r.sel).count();
                ui.horizontal_wrapped(|ui| {
                    if ui.add_enabled(!running && seln > 0, btn_primary(format!("{}  선택 {seln}개 업데이트", ph::ARROWS_CLOCKWISE))).clicked() { do_update = Some((true, false)); }
                    if ui.add_enabled(!running && shown > 0, egui::Button::new(format!("{}  보이는 {shown}개 업데이트", ph::ROCKET_LAUNCH))).clicked() { do_update = Some((true, true)); }
                    if ui.add_enabled(!scanning && seln > 0, egui::Button::new(format!("{}  선택 {seln}개만 스캔", ph::ARROWS_CLOCKWISE))).on_hover_text("선택 사이트만 다시 스캔(전체 스캔 불필요)").clicked() { do_scan_selected = true; }
                    if ui.add_enabled(!running && seln > 0, egui::Button::new(format!("{}  선택 파일·DB 백업", ph::DOWNLOAD_SIMPLE))).on_hover_text("선택 사이트의 파일+DB를 로컬로 내려받기").clicked() { do_backup_sites = true; }
                    if ui.add_enabled(seln > 0, egui::Button::new(format!("{}  DNS 체크(A)", ph::GLOBE_HEMISPHERE_WEST))).on_hover_text("선택 도메인의 A 레코드(IP) 조회").clicked() { do_dns = true; }
                    if ui.add_enabled(seln > 0, egui::Button::new(format!("{}  명령어 보기", ph::FILE_TEXT))).clicked() { do_update = Some((false, false)); }
                });
                ui.label(egui::RichText::new("업데이트는 CMS 유형 자동 분기(WordPress=wp-cli, Rhymix/그누보드=git/오버레이). 상태=최신 버전 비교. DNS 체크로 A 레코드 표시.").weak());
            }
        });
        if let Some(v) = sel_all {
            let fa = self.all_filter_acct.clone();
            let fs = self.all_search.trim().to_lowercase();
            for r in &mut self.all_sites {
                if (fa.is_empty() || r.account == fa) && (fs.is_empty() || r.domain.to_lowercase().contains(&fs)) { r.sel = v; }
            }
        }
        self.load_aliases_batch(alias_loads, ctx);
        if close { self.view = MainView::Domain; }
        if do_load { self.load_all_sites(ctx); }
        if do_rescan_acct { let a = self.all_filter_acct.clone(); self.rescan_account_into_all(a, ctx); }
        if do_dns {
            let doms: Vec<String> = self.all_sites.iter().filter(|r| r.sel).map(|r| r.domain.clone()).collect();
            self.dns_check(doms, ctx);
        }
        if do_scan_selected {
            let pairs: Vec<(String, String)> = self.all_sites.iter().filter(|r| r.sel).map(|r| (r.account.clone(), r.domain.clone())).collect();
            self.rescan_selected(pairs, ctx);
        }
        if do_backup_sites {
            let pairs: Vec<(String, String)> = self.all_sites.iter().filter(|r| r.sel).map(|r| (r.account.clone(), r.domain.clone())).collect();
            self.backup_sites(pairs);
        }
        if let Some((run, all)) = do_update {
            let fa = self.all_filter_acct.clone();
            let fs = self.all_search.trim().to_lowercase();
            let pairs: Vec<(String, String)> = self.all_sites.iter()
                .filter(|r| if all { (fa.is_empty() || r.account == fa) && (fs.is_empty() || r.domain.to_lowercase().contains(&fs)) } else { r.sel })
                .map(|r| (r.account.clone(), r.domain.clone()))
                .collect();
            match ops::build_global_update(&self.store.settings, &pairs) {
                Ok(job) => {
                    if run {
                        self.pending_rescan = pairs.clone();
                        self.eond_confirm = Some(job);
                    }
                    else { self.cmd_view = Some(CmdView { title: job.title.clone(), command: render_command(&job, self.show_pw) }); }
                }
                Err(e) => {
                    self.log.push(format!("전체 업데이트: {e}"));
                    self.status = format!("오류: {e}");
                    self.last_ok = Some(false);
                }
            }
        }
    }

    fn drain_logs(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                LogMsg::Line(l) => {
                    self.log.push(l);
                    if self.log.len() > 1000 {
                        let cut = self.log.len() - 1000;
                        self.log.drain(0..cut);
                    }
                }
                LogMsg::Done { ok } => {
                    self.running = false;
                    self.last_ok = Some(ok);
                    self.status = if ok { "성공".into() } else { "실패 (로그 확인)".into() };
                    // 업데이트 작업 완료 → 선택 사이트 자동 재스캔 예약 (성공 시)
                    if ok && !self.pending_rescan.is_empty() {
                        self.pending_rescan_ready = true;
                    } else {
                        self.pending_rescan.clear();
                    }
                    // 작업 기록을 해당 도메인 이력에 추가
                    if let Some((dom_id, title)) = self.running_job.take() {
                        if let Some(dom) = self.find_domain_mut(dom_id) {
                            dom.history.push(ActivityLog { at: now_unix(), title, ok });
                            if dom.history.len() > 300 {
                                let cut = dom.history.len() - 300;
                                dom.history.drain(0..cut);
                            }
                            self.dirty = true;
                            self.save();
                        }
                    }
                }
                LogMsg::Detected { is_tobe, db } => {
                    self.apply_detected(is_tobe, db);
                }
            }
        }
    }

    /// 접속 테스트로 감지한 DB 정보를 현재 선택된 사이트에 자동 입력
    fn apply_detected(&mut self, is_tobe: bool, db: ops::DetectedDb) {
        let (Some(ci), Some(di)) = (self.sel_customer, self.sel_domain) else { return };
        if ci >= self.store.customers.len() || di >= self.store.customers[ci].domains.len() {
            return;
        }
        let domain = &mut self.store.customers[ci].domains[di];
        let site = if is_tobe { &mut domain.tobe } else { &mut domain.asis };
        let mut filled = Vec::new();
        if let Some(v) = db.name {
            site.db_name = v;
            filled.push("DB이름");
        }
        if let Some(v) = db.user {
            site.db_id = v;
            filled.push("DB아이디");
        }
        if let Some(v) = db.pass {
            site.db_pw = v;
            filled.push("DB비번");
        }
        if let Some(v) = db.host {
            // host:port 또는 host:/socket 형태 분리
            let raw = v.trim();
            if let Some((h, rest)) = raw.split_once(':') {
                if rest.starts_with('/') {
                    site.db_host = rest.to_string(); // 소켓 경로
                } else {
                    site.db_host = h.to_string();
                    site.db_port = rest.to_string();
                }
            } else {
                site.db_host = raw.to_string();
            }
            filled.push("DB호스트");
        }
        if !filled.is_empty() {
            let where_ = if is_tobe { "신규" } else { "현재" };
            let cms = db.cms.unwrap_or_else(|| "알수없음".into());
            self.log.push(format!("[자동입력] {where_} 사이트({cms}) DB 정보 채움: {}", filled.join(", ")));
            self.dirty = true;
            self.save();
        }
    }

    fn start_action(&mut self, action: PendingAction, ctx: &egui::Context) {
        self.last_ok = None;
        let (req, cn, dn, asis, tobe) = action;
        match req {
            Req::Op(kind) => match ops::build(kind, &cn, &dn, &asis, &tobe, None, self.use_root) {
                Ok(job) => {
                    self.running = true;
                    self.status = "작업 실행 중...".into();
                    // 진단(테스트/인증서/점검)은 기록하지 않음, 나머지(백업/복원/직접/수정)는 기록
                    let record = !matches!(
                        kind,
                        OpKind::TestAsis | OpKind::TestTobe | OpKind::CertAsis | OpKind::CertTobe | OpKind::VerifyAsis | OpKind::VerifyTobe
                    );
                    self.running_job = if record {
                        self.current_domain_id().map(|id| (id, job.title.clone()))
                    } else {
                        None
                    };
                    let ctx2 = ctx.clone();
                    let repaint = move || ctx2.request_repaint();
                    match kind {
                        OpKind::TestAsis => ops::spawn_test(job, false, self.tx.clone(), repaint),
                        OpKind::TestTobe => ops::spawn_test(job, true, self.tx.clone(), repaint),
                        _ => ops::spawn(job, self.tx.clone(), repaint),
                    }
                }
                Err(e) => {
                    self.log.push(format!("작업 시작 실패: {e}"));
                    self.status = format!("오류: {e}");
                }
            },
            Req::Migrate(kind) => {
                self.running = true;
                self.status = "묶음 이전 실행 중...".into();
                let title = match kind {
                    MigrateKind::Full => "전체 이전",
                    MigrateKind::FilesOnly => "파일만 이전",
                    MigrateKind::DbOnly => "디비만 이전",
                    MigrateKind::Direct => "전체 직접 이전",
                };
                self.running_job = self.current_domain_id().map(|id| (id, title.to_string()));
                let ctx2 = ctx.clone();
                let steps = match kind {
                    MigrateKind::Full => {
                        vec![OpKind::DbBackup, OpKind::FileBackup, OpKind::DbRestore, OpKind::FileRestore]
                    }
                    MigrateKind::FilesOnly => vec![OpKind::FileBackup, OpKind::FileRestore],
                    MigrateKind::DbOnly => vec![OpKind::DbBackup, OpKind::DbRestore],
                    MigrateKind::Direct => vec![OpKind::DbDirect, OpKind::FileDirect],
                };
                ops::spawn_migration(steps, cn, dn, asis, tobe, self.use_root, self.tx.clone(), move || ctx2.request_repaint());
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_logs();
        self.drain_scans();
        // 업데이트 완료 후 선택 사이트 자동 재스캔
        if self.pending_rescan_ready && !self.scanning {
            self.pending_rescan_ready = false;
            let p = std::mem::take(&mut self.pending_rescan);
            self.rescan_selected(p, ctx);
        }

        if self.locked {
            self.lock_screen(ctx);
            return;
        }

        if self.confirm.is_some() {
            self.confirm_modal(ctx);
        }
        if self.cmd_view.is_some() {
            self.cmd_view_modal(ctx);
        }
        if self.eond_confirm.is_some() {
            self.eond_confirm_modal(ctx);
        }
        if self.pending_delete.is_some() {
            self.delete_modal(ctx);
        }

        self.top_bar(ctx);
        self.left_panel(ctx);
        if let Some(ci) = self.pending_import_sites.take() {
            self.hestia_import_sites(ci);
        }
        self.bottom_log(ctx);
        match self.view {
            MainView::Settings => self.settings_page(ctx),
            MainView::AccountModules(ci) => self.account_modules_page(ctx, ci),
            MainView::AllSites => self.all_sites_page(ctx),
            MainView::Domain => self.central(ctx),
        }

        // 화면 위치(고객/도메인/뷰/탭)가 바뀌면 자동저장 트리거 — 재시작 시 마지막 화면 복원용.
        if !self.locked && self.store.ui != self.current_ui() {
            self.dirty = true;
            self.last_edit = ctx.input(|i| i.time);
        }

        // 디바운스 자동 저장: Argon2 KDF 가 무거워 매 키 입력마다 저장하지 않고,
        // 입력이 멈춘 뒤 약 0.6초 후 저장한다.
        if self.dirty {
            let now = ctx.input(|i| i.time);
            if now - self.last_edit > 0.6 {
                self.save();
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(250));
            }
        }
    }
}

impl App {
    fn lock_screen(&mut self, ctx: &egui::Context) {
        let total_w = LOCK_W + 2.0 * LOCK_PAD.x; // 인풋 전체 너비(버튼과 정렬)
        let btn = |label: &str| egui::Button::new(label).min_size(egui::vec2(total_w, LOCK_H));
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(70.0);
            ui.vertical_centered(|ui| {
                ui.heading("Hostmover");
                ui.add_space(4.0);
                ui.label(egui::RichText::new("호스팅 이전 백업/복원 관리").weak());
                ui.add_space(18.0);
                // 화면 진입 시 포커스가 없으면 입력칸에 커서를 둔다
                let nothing_focused = ui.memory(|m| m.focused().is_none());
                let vis = self.show_lock_pw;

                if self.creating {
                    ui.label("새 마스터 패스워드를 설정하세요");
                    ui.add_space(10.0);
                    let r1 = ui.add(pw_field(&mut self.password, "마스터 패스워드", vis));
                    ui.add_space(10.0);
                    ui.add(pw_field(&mut self.password_confirm, "패스워드 확인", vis));
                    if nothing_focused {
                        r1.request_focus();
                    }
                    ui.add_space(8.0);
                    ui.checkbox(&mut self.show_lock_pw, "비밀번호 표시");
                    ui.add_space(10.0);
                    if ui.add(btn("생성")).clicked() {
                        if self.password.is_empty() {
                            self.auth_error = "패스워드를 입력하세요".into();
                        } else if self.password != self.password_confirm {
                            self.auth_error = "패스워드가 일치하지 않습니다".into();
                        } else {
                            match store::save(&self.password, &Store::default()) {
                                Ok(_) => {
                                    self.master_pw = std::mem::take(&mut self.password);
                                    self.store = Store::default();
                                    self.locked = false;
                                    self.password_confirm.clear();
                                }
                                Err(e) => self.auth_error = e,
                            }
                        }
                    }
                } else {
                    ui.label("마스터 패스워드를 입력하세요");
                    ui.add_space(10.0);
                    let resp = ui.add(pw_field(&mut self.password, "마스터 패스워드", vis));
                    if nothing_focused {
                        resp.request_focus();
                    }
                    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.add_space(8.0);
                    ui.checkbox(&mut self.show_lock_pw, "비밀번호 표시");
                    ui.add_space(10.0);
                    if ui.add(btn("잠금 해제")).clicked() || enter {
                        match store::load(&self.password) {
                            Ok(s) => {
                                self.store = s;
                                self.master_pw = std::mem::take(&mut self.password);
                                self.locked = false;
                                self.purge_old_trash();
                                self.hydrate_scan_cache();
                                self.restore_ui();
                            }
                            Err(e) => self.auth_error = e,
                        }
                    }
                }

                if !self.auth_error.is_empty() {
                    ui.add_space(8.0);
                    ui.colored_label(egui::Color32::from_rgb(242, 109, 109), &self.auth_error);
                }
            });
        });
    }

    fn top_bar(&mut self, ctx: &egui::Context) {
        let frame = egui::Frame::side_top_panel(&ctx.style()).inner_margin(egui::Margin::symmetric(14, 9));
        egui::TopBottomPanel::top("top").frame(frame).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Hostmover");
                ui.separator();
                if ui.button(format!("{}  저장", ph::FLOPPY_DISK)).clicked() {
                    self.save();
                }
                ui.weak(if self.dirty { "자동저장 대기…" } else { "자동저장됨 ✓" });
                ui.separator();
                ui.checkbox(&mut self.show_pw, "비밀번호 표시");
                ui.checkbox(&mut self.use_root, "루트로 실행")
                    .on_hover_text("켜면 서버루트 계정(있으면)으로 SSH 접속. 명령어 보기에도 반영됨");
                ui.separator();
                let in_all = self.view == MainView::AllSites;
                if ui.selectable_label(in_all, format!("{}  전체 사이트", ph::LIST_BULLETS)).on_hover_text("모든 계정의 사이트 CMS/버전/용량 통합 리스트 + 일괄 업데이트").clicked() {
                    self.view = if in_all { MainView::Domain } else { MainView::AllSites };
                }
                let in_settings = self.view == MainView::Settings;
                if ui.selectable_label(in_settings, format!("{}  설정", ph::GEAR)).on_hover_text("HestiaCP 연동 / 서버 SSH / 일괄 업데이트 / 모듈 일괄삭제").clicked() {
                    self.view = if in_settings { MainView::Domain } else { MainView::Settings };
                }
                ui.separator();
                if self.running {
                    ui.spinner();
                    ui.label("실행 중");
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    match self.last_ok {
                        Some(true) => {
                            ui.label(
                                egui::RichText::new(format!("  ✓ {}  ", self.status))
                                    .size(16.0)
                                    .strong()
                                    .color(egui::Color32::WHITE)
                                    .background_color(egui::Color32::from_rgb(34, 150, 74)),
                            );
                        }
                        Some(false) => {
                            ui.label(
                                egui::RichText::new(format!("  ✗ {}  ", self.status))
                                    .size(16.0)
                                    .strong()
                                    .color(egui::Color32::WHITE)
                                    .background_color(egui::Color32::from_rgb(190, 60, 60)),
                            );
                        }
                        None => {
                            ui.label(&self.status);
                        }
                    }
                });
            });
        });
    }

    fn left_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("tree").resizable(true).default_width(252.0).show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.new_customer).hint_text("새 고객명").desired_width(140.0).margin(FIELD_MARGIN));
                if ui.button("+ 고객").clicked() && !self.new_customer.trim().is_empty() {
                    let id = self.store.alloc_id();
                    self.store.customers.push(Customer {
                        id,
                        name: self.new_customer.trim().to_string(),
                        memo: String::new(),
                        domains: Vec::new(),
                        deleted_at: None,
                    });
                    self.new_customer.clear();
                    self.dirty = true;
                    self.save();
                }
            });
            let tn = self.trash_count();
            if ui.selectable_label(self.show_trash, format!("🗑 휴지통 ({tn})")).clicked() {
                self.show_trash = !self.show_trash;
            }
            ui.separator();
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                if self.show_trash {
                    self.trash_view(ui);
                } else {
                    self.tree_view(ui);
                }
            });
        });
    }

    /// 고객→도메인 트리 (삭제되지 않은 것만)
    fn tree_view(&mut self, ui: &mut egui::Ui) {
        let mut select: Option<(usize, usize)> = None;
        let mut open_account: Option<usize> = None;
        for ci in 0..self.store.customers.len() {
            if self.store.customers[ci].deleted_at.is_some() {
                continue;
            }
            let cust_id = self.store.customers[ci].id;
            let name = self.store.customers[ci].name.clone();
            let collapsed = self.collapsed.contains(&cust_id);
            ui.horizontal(|ui| {
                let caret = if collapsed { ph::CARET_RIGHT } else { ph::CARET_DOWN };
                if ui.add(egui::Button::new(egui::RichText::new(caret).size(15.0)).frame(false)).clicked() {
                    if collapsed { self.collapsed.remove(&cust_id); } else { self.collapsed.insert(cust_id); }
                }
                if ui.add(egui::Button::new(egui::RichText::new(format!("{}  {name}", ph::BUILDINGS)).strong()).frame(false))
                    .on_hover_text("계정 관리 열기 (사이트 CMS/버전·일괄 업데이트, 모듈)").clicked() {
                    open_account = Some(ci);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let icon = |ui: &mut egui::Ui, t: &str, col: Option<egui::Color32>, hint: &str| {
                        let mut rt = egui::RichText::new(t).size(17.0);
                        if let Some(c) = col { rt = rt.color(c); }
                        ui.add(egui::Button::new(rt).frame(false)).on_hover_text(hint).clicked()
                    };
                    if icon(ui, ph::TRASH, Some(egui::Color32::from_rgb(206, 112, 112)), "이 고객 삭제(휴지통)") {
                        self.pending_delete = Some(DelTarget::Customer(ci));
                    }
                    if icon(ui, ph::DOWNLOAD_SIMPLE, None, "HestiaCP 사이트 불러오기") {
                        self.pending_import_sites = Some(ci);
                    }
                    if icon(ui, ph::PUZZLE_PIECE, None, "계정 관리 (사이트 CMS/버전·일괄 업데이트, 모듈 삭제)") {
                        open_account = Some(ci);
                    }
                    if icon(ui, ph::PLUS, None, "새 도메인 추가") {
                        if self.add_open.contains(&cust_id) { self.add_open.remove(&cust_id); } else { self.add_open.insert(cust_id); }
                    }
                });
            });
            if collapsed {
                continue;
            }
            ui.indent(("c", cust_id), |ui| {
                // 새 도메인 입력칸은 목록 상단에
                if self.add_open.contains(&cust_id) {
                    ui.horizontal(|ui| {
                        let buf = self.new_domain.entry(cust_id).or_default();
                        let resp = ui.add(egui::TextEdit::singleline(buf).hint_text("새 도메인").desired_width(120.0).margin(FIELD_MARGIN));
                        let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        let add = (ui.button("추가").clicked() || enter) && !buf.trim().is_empty();
                        let name = if add {
                            let n = buf.trim().to_string();
                            buf.clear();
                            n
                        } else {
                            String::new()
                        };
                        if add {
                            let id = self.store.alloc_id();
                            self.store.customers[ci].domains.insert(0, Domain {
                                id,
                                name,
                                memo: String::new(),
                                access: DomainAccess::default(),
                                asis: Site::default(),
                                tobe: Site::default(),
                                cms: CmsAccess::default(),
                                eond: Default::default(),
                                cms_install: Default::default(),
                                deleted_at: None,
                                history: Vec::new(),
                            });
                            self.sel_customer = Some(ci);
                            self.sel_domain = Some(0);
                            self.dirty = true;
                            self.save();
                        }
                    });
                }
                let dlen = self.store.customers[ci].domains.len();
                for di in 0..dlen {
                    if self.store.customers[ci].domains[di].deleted_at.is_some() {
                        continue;
                    }
                    let dname = self.store.customers[ci].domains[di].name.clone();
                    let selected = self.sel_customer == Some(ci) && self.sel_domain == Some(di);
                    if ui.selectable_label(selected, format!("{}  {dname}", ph::GLOBE)).clicked() {
                        select = Some((ci, di));
                    }
                }
            });
        }
        if let Some((ci, di)) = select {
            self.sel_customer = Some(ci);
            self.sel_domain = Some(di);
            // 설정/계정모듈 화면에서 도메인 선택 시 도메인 화면으로 복귀
            self.view = MainView::Domain;
        }
        if let Some(ci) = open_account {
            self.open_account_page(ci);
        }
    }

    /// 계정 관리 페이지 열기 — 사이트 탭. 사이트 데이터는 공유 캐시(all_sites)를 계정으로 필터링해 표시.
    fn open_account_page(&mut self, ci: usize) {
        self.sel_customer = Some(ci);
        self.acct_tab = AcctTab::Sites;
        self.acct_mods.clear();
        self.acct_mods_status.clear();
        self.acct_mods_for = Some(ci);
        self.view = MainView::AccountModules(ci);
    }

    /// 휴지통: 삭제된 고객/도메인 + 복원/완전삭제 + 남은 일수
    fn trash_view(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new("삭제 항목 — 30일 후 자동 완전삭제").weak());
        ui.add_space(4.0);
        let mut restore: Option<DelTarget> = None;
        let mut purge: Option<DelTarget> = None;
        let mut any = false;
        for ci in 0..self.store.customers.len() {
            if let Some(ts) = self.store.customers[ci].deleted_at {
                any = true;
                let name = self.store.customers[ci].name.clone();
                ui.horizontal(|ui| {
                    ui.label(format!("{}  {name}", ph::BUILDINGS));
                    ui.weak(format!("{}일", remaining_days(ts)));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("완전삭제").clicked() { purge = Some(DelTarget::Customer(ci)); }
                        if ui.small_button("복원").clicked() { restore = Some(DelTarget::Customer(ci)); }
                    });
                });
            } else {
                for di in 0..self.store.customers[ci].domains.len() {
                    if let Some(ts) = self.store.customers[ci].domains[di].deleted_at {
                        any = true;
                        let cn = self.store.customers[ci].name.clone();
                        let dn = self.store.customers[ci].domains[di].name.clone();
                        ui.horizontal(|ui| {
                            ui.label(format!("{}  {dn}", ph::GLOBE));
                            ui.weak(format!("{cn} · {}일", remaining_days(ts)));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.small_button("완전삭제").clicked() { purge = Some(DelTarget::Domain(ci, di)); }
                                if ui.small_button("복원").clicked() { restore = Some(DelTarget::Domain(ci, di)); }
                            });
                        });
                    }
                }
            }
        }
        if !any {
            ui.weak("(비어 있음)");
        }
        if let Some(t) = restore {
            match t {
                DelTarget::Customer(ci) => self.store.customers[ci].deleted_at = None,
                DelTarget::Domain(ci, di) => self.store.customers[ci].domains[di].deleted_at = None,
            }
            self.dirty = true;
            self.save();
        }
        if let Some(t) = purge {
            match t {
                DelTarget::Customer(ci) => { self.store.customers.remove(ci); }
                DelTarget::Domain(ci, di) => { self.store.customers[ci].domains.remove(di); }
            }
            self.sel_customer = None;
            self.sel_domain = None;
            self.dirty = true;
            self.save();
        }
    }

    fn trash_count(&self) -> usize {
        let mut n = 0;
        for c in &self.store.customers {
            if c.deleted_at.is_some() {
                n += 1;
            } else {
                n += c.domains.iter().filter(|d| d.deleted_at.is_some()).count();
            }
        }
        n
    }

    /// 30일 지난 휴지통 항목 완전삭제 (잠금 해제 시 1회)
    fn purge_old_trash(&mut self) {
        let cutoff = now_unix() - TRASH_RETENTION_SECS;
        let mut changed = false;
        for c in &mut self.store.customers {
            let before = c.domains.len();
            c.domains.retain(|d| d.deleted_at.map_or(true, |t| t > cutoff));
            if c.domains.len() != before {
                changed = true;
            }
        }
        let before = self.store.customers.len();
        self.store.customers.retain(|c| c.deleted_at.map_or(true, |t| t > cutoff));
        if self.store.customers.len() != before {
            changed = true;
        }
        if changed {
            self.sel_customer = None;
            self.sel_domain = None;
            self.dirty = true;
            self.save();
        }
    }

    /// 삭제 확인 모달 (휴지통으로 이동)
    fn delete_modal(&mut self, ctx: &egui::Context) {
        let Some(target) = self.pending_delete else { return };
        let (title, msg) = match target {
            DelTarget::Customer(ci) => {
                let name = self.store.customers.get(ci).map(|c| c.name.clone()).unwrap_or_default();
                ("고객 삭제", format!("고객 '{name}' 와 하위 도메인 전체를 휴지통으로 이동합니다."))
            }
            DelTarget::Domain(ci, di) => {
                let name = self.store.customers.get(ci).and_then(|c| c.domains.get(di)).map(|d| d.name.clone()).unwrap_or_default();
                ("도메인 삭제", format!("도메인 '{name}' 을 휴지통으로 이동합니다."))
            }
        };
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(msg);
                ui.colored_label(egui::Color32::from_rgb(220, 140, 60), "30일 후 완전삭제됩니다. 그 전엔 휴지통에서 복원 가능.");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.add(btn_danger(format!("{}  휴지통으로 이동", ph::TRASH))).clicked() {
                        let now = now_unix();
                        match target {
                            DelTarget::Customer(ci) => {
                                if let Some(c) = self.store.customers.get_mut(ci) { c.deleted_at = Some(now); }
                                if self.sel_customer == Some(ci) { self.sel_customer = None; self.sel_domain = None; }
                            }
                            DelTarget::Domain(ci, di) => {
                                if let Some(d) = self.store.customers.get_mut(ci).and_then(|c| c.domains.get_mut(di)) { d.deleted_at = Some(now); }
                                if self.sel_customer == Some(ci) && self.sel_domain == Some(di) { self.sel_domain = None; }
                            }
                        }
                        self.pending_delete = None;
                        self.dirty = true;
                        self.save();
                    }
                    if ui.button("취소").clicked() {
                        self.pending_delete = None;
                    }
                });
            });
    }

    fn bottom_log(&mut self, ctx: &egui::Context) {
        let frame = egui::Frame::side_top_panel(&ctx.style()).inner_margin(egui::Margin::symmetric(14, 10));
        egui::TopBottomPanel::bottom("log").resizable(true).default_height(186.0).frame(frame).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.strong("작업 로그");
                ui.weak(format!("({}줄)", self.log.len()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(format!("{}  지우기", ph::ERASER)).clicked() {
                        self.log.clear();
                    }
                    if ui.button(format!("{}  전체 복사", ph::COPY)).on_hover_text("작업 로그 전체를 클립보드로 복사").clicked() {
                        ui.ctx().copy_text(self.log.join("\n"));
                        self.status = "로그 복사됨".into();
                    }
                });
            });
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);
            egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
                for line in &self.log {
                    let txt = egui::RichText::new(line).monospace();
                    if is_success_line(line) {
                        ui.label(txt.color(egui::Color32::from_rgb(74, 200, 120)).strong());
                    } else if is_error_line(line) {
                        ui.label(txt.color(egui::Color32::from_rgb(236, 110, 110)));
                    } else {
                        ui.label(txt);
                    }
                }
            });
        });
    }

    fn central(&mut self, ctx: &egui::Context) {
        let frame = egui::Frame::central_panel(&ctx.style()).inner_margin(egui::Margin::symmetric(14, 12));
        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            let (ci, di) = match (self.sel_customer, self.sel_domain) {
                (Some(c), Some(d))
                    if c < self.store.customers.len()
                        && d < self.store.customers[c].domains.len()
                        && self.store.customers[c].deleted_at.is_none()
                        && self.store.customers[c].domains[d].deleted_at.is_none() =>
                {
                    (c, d)
                }
                _ => {
                    ui.centered_and_justified(|ui| {
                        ui.label("좌측에서 도메인을 선택하거나 새로 추가하세요");
                    });
                    return;
                }
            };

            let show_pw = self.show_pw;
            let running = self.running;
            let customer_name = self.store.customers[ci].name.clone();
            // domain 가변차용 전에 미리 복사 (이후 self.store 접근 불가)
            let rx_source = self.store.settings.rx_source_local.clone();
            let wp_source = self.store.settings.wp_source_local.clone();
            let wp_scanning = self.wp_scanning;
            let wp_scan_status = self.wp_scan_status.clone();
            // 캐시에서 이 도메인의 플러그인·테마 행을 미리 클론(토글 후 현재 종류로 선택)
            let did0 = self.store.customers[ci].domains[di].id;
            let wp_rows_plugin = self.wp_cache.get(&(did0, ops::WpAssetKind::Plugin)).cloned().unwrap_or_default();
            let wp_rows_theme = self.wp_cache.get(&(did0, ops::WpAssetKind::Theme)).cloned().unwrap_or_default();
            let mut wp_kind = self.wp_kind;
            let mut wp_filter = self.wp_filter.clone();
            let mut wp_sel_local = self.wp_sel.clone();
            let mut wp_scan_req = false;
            let mut wp_upload_req: Option<bool> = None;
            let mut wp_download_req: Option<bool> = None;
            let mut changed = false;
            let mut request: Option<PendingAction> = None;
            let mut view_request: Option<(OpKind, String, String, Site, Site)> = None;
            // eondcms 설치: (단계 1~3, 실행여부) — 실행이면 true, 명령어보기면 false
            let mut eond_step: Option<(u8, bool)> = None;
            // CMS 설치: (1=설치/2=업데이트, 실행여부)
            let mut cms_step: Option<(u8, bool)> = None;
            // Rhymix 모듈/레이아웃 업로드: 실행여부(true=실행, false=명령어보기)
            let mut rx_upload: Option<bool> = None;
            let mut dryrun = false;
            let mut delete_domain = false;

            let domain = &mut self.store.customers[ci].domains[di];
            let domain_name = domain.name.clone();
            // 도메인마다 위젯 ID를 분리해 편집 상태(커서/IME)가 도메인 간 공유되는 버그 방지
            let did = domain.id;

            let mut tab = self.tab;

            // ── 헤더 (항상 표시) ──
            ui.horizontal(|ui| {
                ui.heading(format!("{}  {}", ph::GLOBE, domain.name));
                if let Some(p) = puny_if_different(&domain.name) {
                    ui.add(egui::Label::new(egui::RichText::new(&p).monospace().weak()).selectable(true))
                        .on_hover_text("퓨니코드 (드래그 복사)");
                }
                ui.label(egui::RichText::new(format!("· {customer_name}")).weak());
                if ui.button(format!("{}  DNS", ph::GLOBE_HEMISPHERE_WEST)).on_hover_text("whatsmydns.net 에서 A 레코드 전세계 전파 조회").clicked() {
                    let host = to_punycode(&domain.name).unwrap_or_else(|| domain.name.trim().to_string());
                    if !host.is_empty() {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(format!("https://www.whatsmydns.net/#A/{host}")));
                    }
                }
                if ui.button(format!("{}  드라이런", ph::MAGNIFYING_GLASS)).on_hover_text("실행하지 않고 입력값/작업 준비상태를 점검").clicked() {
                    dryrun = true;
                }
                if ui.button(format!("{}  브라우저로 열기", ph::ARROW_SQUARE_OUT)).on_hover_text("이 도메인을 웹 브라우저에서 열기").clicked() {
                    let host = to_punycode(&domain.name).unwrap_or_else(|| domain.name.trim().to_string());
                    if !host.is_empty() {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(format!("https://{host}")));
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(format!("{}  삭제", ph::TRASH)).on_hover_text("이 도메인을 휴지통으로 (확인 후)").clicked() {
                        delete_domain = true;
                    }
                });
            });
            // ── 탭 바 ──
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                for (t, label) in [
                    (Tab::Info, "  정보  "),
                    (Tab::Migrate, "  사이트이전  "),
                    (Tab::Cms, "  CMS설치  "),
                    (Tab::Eond, "  eondcms  "),
                    (Tab::History, "  기록  "),
                ] {
                    if ui.selectable_label(tab == t, label).clicked() {
                        tab = t;
                    }
                }
            });
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(10.0);

            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
              ui.push_id(("domain", did), |ui| {
                match tab {
                    // ───────── 정보 입력 (좌우 배치로 세로 길이 축소) ─────────
                    Tab::Info => {
                        ui.horizontal(|ui| {
                            ui.label("메모");
                            changed |= ui.add(egui::TextEdit::singleline(&mut domain.memo).desired_width(440.0).margin(FIELD_MARGIN)).changed();
                        });
                        ui.add_space(6.0);
                        // ① 도메인 접속 / ④ CMS 좌우
                        ui.columns(2, |cols| {
                            card(&mut cols[0], |ui| {
                                ui.strong("① 도메인 접속정보 (네임서버 수동)");
                                changed |= access_editor(ui, &mut domain.access, show_pw);
                            });
                            card(&mut cols[1], |ui| {
                                ui.strong("④ CMS 접속정보");
                                changed |= cms_editor(ui, &mut domain.cms, show_pw);
                            });
                        });
                        ui.add_space(6.0);
                        // ② 현재 / ③ 신규 좌우
                        ui.columns(2, |cols| {
                            card(&mut cols[0], |ui| {
                                ui.strong("② 현재 사이트 (ASIS)");
                                changed |= site_fields(ui, &mut domain.asis, show_pw);
                                ui.add_space(4.0);
                                if ui.add_enabled(!running, egui::Button::new(format!("{}  접속 테스트", ph::PLUGS_CONNECTED)))
                                    .on_hover_text("SSH 로그인 + 원격 도구(mysqldump/rsync/tar)·DB소켓·CMS설정 탐지 → DB정보 자동입력").clicked() {
                                    request = Some((Req::Op(OpKind::TestAsis), customer_name.clone(), domain_name.clone(), domain.asis.clone(), domain.tobe.clone()));
                                }
                            });
                            card(&mut cols[1], |ui| {
                                ui.strong("③ 신규 사이트 (TOBE)");
                                changed |= site_fields(ui, &mut domain.tobe, show_pw);
                                ui.add_space(4.0);
                                if ui.add_enabled(!running, egui::Button::new(format!("{}  접속 테스트", ph::PLUGS_CONNECTED)))
                                    .on_hover_text("SSH 로그인 + 원격 도구(mysqldump/rsync/tar)·DB소켓·CMS설정 탐지 → DB정보 자동입력").clicked() {
                                    request = Some((Req::Op(OpKind::TestTobe), customer_name.clone(), domain_name.clone(), domain.asis.clone(), domain.tobe.clone()));
                                }
                            });
                        });
                    }
                    // ───────── 이전·진단 (통합, 좌우 배치) ─────────
                    Tab::Migrate => {
                        // 진단·수정 — 사이트별 액션 (좌우)
                        card(ui, |ui| {
                            ui.strong("🩺 진단·수정");
                            ui.columns(2, |cols| {
                                card(&mut cols[0], |ui| {
                                    ui.strong("② 현재 (ASIS)");
                                    let a = site_actions(ui, !running);
                                    let mk = |k| Some((Req::Op(k), customer_name.clone(), domain_name.clone(), domain.asis.clone(), domain.tobe.clone()));
                                    if a.cert { request = mk(OpKind::CertAsis); }
                                    if a.verify { request = mk(OpKind::VerifyAsis); }
                                    if a.fix_htaccess { request = mk(OpKind::FixHtaccessAsis); }
                                    if a.set_db { request = mk(OpKind::SetDbAsis); }
                                });
                                card(&mut cols[1], |ui| {
                                    ui.strong("③ 신규 (TOBE)");
                                    let a = site_actions(ui, !running);
                                    let mk = |k| Some((Req::Op(k), customer_name.clone(), domain_name.clone(), domain.asis.clone(), domain.tobe.clone()));
                                    if a.cert { request = mk(OpKind::CertTobe); }
                                    if a.verify { request = mk(OpKind::VerifyTobe); }
                                    if a.fix_htaccess { request = mk(OpKind::FixHtaccessTobe); }
                                    if a.set_db { request = mk(OpKind::SetDbTobe); }
                                });
                            });
                        });
                        ui.add_space(6.0);
                        let asis = domain.asis.clone();
                        let tobe = domain.tobe.clone();
                        // 마이그레이션 — 좌(개별) / 우(묶음 + 직접)
                        ui.columns(2, |cols| {
                            card(&mut cols[0], |ui| {
                                ui.strong("개별 작업");
                                ui.label(egui::RichText::new("백업: 현재→로컬 / 복원: 로컬→신규").weak());
                                ui.add_space(4.0);
                                for (label, kind) in [
                                    (format!("{}  DB 백업", ph::ARROW_LINE_DOWN), OpKind::DbBackup),
                                    (format!("{}  파일 백업", ph::ARROW_LINE_DOWN), OpKind::FileBackup),
                                    (format!("{}  DB 복원", ph::ARROW_LINE_UP), OpKind::DbRestore),
                                    (format!("{}  파일 복원", ph::ARROW_LINE_UP), OpKind::FileRestore),
                                ] {
                                    ui.horizontal(|ui| {
                                        if ui.add_enabled(!running, egui::Button::new(&label).min_size(egui::vec2(132.0, 0.0))).clicked() {
                                            request = Some((Req::Op(kind), customer_name.clone(), domain_name.clone(), asis.clone(), tobe.clone()));
                                        }
                                        if ui.button(ph::FILE_TEXT).on_hover_text("명령어만 보기/복사").clicked() {
                                            view_request = Some((kind, customer_name.clone(), domain_name.clone(), asis.clone(), tobe.clone()));
                                        }
                                    });
                                }
                            });
                            cols[1].vertical(|ui| {
                                card(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.strong("묶음 이전 (현재 → 신규)");
                                        ui.label(egui::RichText::new("· rsync (실패 시 tar 폴백)").weak().small())
                                            .on_hover_text("파일: rsync 우선, 원격에 rsync 없으면 tar-over-ssh 폴백 · DB: mysqldump");
                                    });
                                    ui.label(egui::RichText::new("현재→로컬 디스크에 백업본 저장 후→신규로 복원. 백업본이 남아 재시도·롤백이 안전하나, 로컬 디스크 공간이 필요하고 전송이 2회라 느립니다.").weak());
                                    ui.label(egui::RichText::new("※ 공간 부족 시 '파일만' → 확보 후 '디비만'").weak());
                                    ui.add_space(4.0);
                                    ui.horizontal_wrapped(|ui| {
                                        if ui.add_enabled(!running, btn_go(format!("{}  전체 이전", ph::ROCKET_LAUNCH))).clicked() {
                                            request = Some((Req::Migrate(MigrateKind::Full), customer_name.clone(), domain_name.clone(), asis.clone(), tobe.clone()));
                                        }
                                        if ui.add_enabled(!running, egui::Button::new(format!("{}  파일만", ph::FOLDER))).clicked() {
                                            request = Some((Req::Migrate(MigrateKind::FilesOnly), customer_name.clone(), domain_name.clone(), asis.clone(), tobe.clone()));
                                        }
                                        if ui.add_enabled(!running, egui::Button::new(format!("{}  디비만", ph::DATABASE))).clicked() {
                                            request = Some((Req::Migrate(MigrateKind::DbOnly), customer_name.clone(), domain_name.clone(), asis.clone(), tobe.clone()));
                                        }
                                    });
                                });
                                ui.add_space(6.0);
                                card(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.strong(format!("{}  직접 이전 (디스크 미사용)", ph::LIGHTNING));
                                        ui.label(egui::RichText::new("· tar 파이프").weak().small())
                                            .on_hover_text("파일: tar -czf - . | ssh | tar -xzf - (무저장 스트리밍) · DB: mysqldump 파이프");
                                    });
                                    ui.label(egui::RichText::new("로컬 디스크를 안 거치고 현재→신규로 바로 스트리밍. 전송 1회라 빠르고 디스크가 필요 없으나, 백업본이 남지 않아 중간 실패 시 처음부터 다시 합니다.").weak());
                                    ui.add_space(4.0);
                                    ui.horizontal_wrapped(|ui| {
                                        if ui.add_enabled(!running, egui::Button::new(format!("{}  DB 직접", ph::LIGHTNING))).clicked() {
                                            request = Some((Req::Op(OpKind::DbDirect), customer_name.clone(), domain_name.clone(), asis.clone(), tobe.clone()));
                                        }
                                        if ui.add_enabled(!running, egui::Button::new(format!("{}  파일 직접", ph::LIGHTNING))).clicked() {
                                            request = Some((Req::Op(OpKind::FileDirect), customer_name.clone(), domain_name.clone(), asis.clone(), tobe.clone()));
                                        }
                                        if ui.add_enabled(!running, btn_primary(format!("{}  전체 직접", ph::LIGHTNING))).clicked() {
                                            request = Some((Req::Migrate(MigrateKind::Direct), customer_name.clone(), domain_name.clone(), asis.clone(), tobe.clone()));
                                        }
                                    });
                                });
                            });
                        });
                    }
                    // ───────── CMS 설치 (WordPress/Rhymix/그누보드) ─────────
                    Tab::Cms => {
                        let asis_ip = if domain.asis.ip.trim().is_empty() { "(IP 빔)".to_string() } else { domain.asis.ip.trim().to_string() };
                        let tobe_ip = if domain.tobe.ip.trim().is_empty() { "(IP 빔)".to_string() } else { domain.tobe.ip.trim().to_string() };
                        // 정보 탭에서 가져올 값(읽기 전용 표시) — 선택 서버 DB + ④ CMS 접속정보(관리자)
                        let blank = |s: &str| if s.trim().is_empty() { "(빔)".to_string() } else { s.trim().to_string() };
                        let mask = |s: &str| if s.is_empty() { "(빔)".to_string() } else { "●●●●".to_string() };
                        let sel = if domain.cms_install.use_asis { &domain.asis } else { &domain.tobe };
                        let hestia_disp = format!("{} / {}", blank(&sel.ftp_id), mask(&sel.ftp_pw));
                        let db_disp = format!("{} / {} / {}", blank(&sel.db_name), blank(&sel.db_id), mask(&sel.db_pw));
                        let admin_disp = format!("{} / {}", blank(&domain.cms.id), mask(&domain.cms.pw));
                        let c = &mut domain.cms_install;
                        card(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.strong("CMS:");
                                changed |= ui.radio_value(&mut c.kind, CmsKind::WordPress, "WordPress").changed();
                                changed |= ui.radio_value(&mut c.kind, CmsKind::Rhymix, "Rhymix").changed();
                                changed |= ui.radio_value(&mut c.kind, CmsKind::Gnuboard, "그누보드").changed();
                            });
                            ui.horizontal(|ui| {
                                ui.label("대상 서버:");
                                changed |= ui.radio_value(&mut c.use_asis, true, format!("현재(ASIS) {asis_ip}")).changed();
                                changed |= ui.radio_value(&mut c.use_asis, false, format!("신규(TOBE) {tobe_ip}")).changed();
                            });
                            changed |= ui.checkbox(&mut c.sudo, "sudo 경유 (root 직접 로그인 불가 → tong 등으로 접속 후 sudo)").changed();
                            egui::Grid::new(("cmsinstall", ui.next_auto_id())).num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                                changed |= row_text_hint(ui, "유저 이메일", &mut c.hestia_email, "HestiaCP 유저 생성에 필요(설치 시)");
                                changed |= row_text_hint(ui, "패키지", &mut c.package, "HestiaCP 패키지(보통 default)");
                                changed |= row_text_hint(ui, "관리자 이메일", &mut c.admin_email, "WordPress·Rhymix 필수(관리자 자동생성)");
                                changed |= row_text_hint(ui, "사이트 제목", &mut c.site_title, "WordPress 사이트 제목(기본 My Site)");
                                changed |= row_text_hint(ui, "언어", &mut c.locale, "기본 ko_KR");
                                changed |= row_text_hint(ui, "버전", &mut c.version, "기본 latest (예: 6.5)");
                            });
                            ui.add_space(6.0);
                            // DB·관리자 정보는 정보 탭을 단일 출처로 사용 (여기선 읽기 전용 표시)
                            egui::Grid::new(("cmsinstall_ro", ui.next_auto_id())).num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                                grid_label(ui, "HestiaCP 유저 (=FTP)");
                                ui.label(egui::RichText::new(&hestia_disp).monospace())
                                    .on_hover_text("선택한 대상 서버의 FTP 계정/비번 = HestiaCP vhost 유저 (정보 탭 ②현재·③신규에서 수정). 업데이트는 이 계정으로 root 없이 실행");
                                ui.end_row();
                                grid_label(ui, "DB (정보 탭)");
                                ui.label(egui::RichText::new(&db_disp).monospace())
                                    .on_hover_text("선택한 대상 서버의 DB이름/유저/비번 (정보 탭 ②현재·③신규에서 수정)");
                                ui.end_row();
                                grid_label(ui, "관리자 (④ CMS)");
                                ui.label(egui::RichText::new(&admin_disp).monospace())
                                    .on_hover_text("④ CMS 접속정보의 ID/비번 (정보 탭에서 수정)");
                                ui.end_row();
                            });
                            ui.label(egui::RichText::new("※ HestiaCP 유저·DB·관리자 정보는 정보 탭에서 수정합니다. (대상 서버 = 선택된 현재/신규 서버)").weak());
                        });
                        ui.add_space(6.0);
                        card(ui, |ui| {
                            ui.strong(format!("{}  ('루트로 실행' 필수)", c.kind.label()));
                            let hint = match c.kind {
                                CmsKind::WordPress => "설치: wp-cli 코어+DB+관리자(완전 자동)  /  업데이트: 코어·플러그인·테마·언어",
                                CmsKind::Rhymix => "설치: git clone+DB+권한+SSL+무인설치(procInstall, 관리자 자동생성·마법사 없음)  /  업데이트: git pull",
                                CmsKind::Gnuboard => "설치: git clone+DB+data권한+SSL → /install/ 마법사  /  업데이트: git pull",
                            };
                            ui.label(egui::RichText::new(hint).weak());
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                if ui.add_enabled(!running, btn_primary(format!("{}  설치", ph::DOWNLOAD)).min_size(egui::vec2(132.0, 0.0))).clicked() {
                                    cms_step = Some((1, true));
                                }
                                if ui.button(ph::FILE_TEXT).on_hover_text("설치 명령어 보기").clicked() {
                                    cms_step = Some((1, false));
                                }
                                if ui.add_enabled(!running, egui::Button::new(format!("{}  업데이트", ph::ARROWS_CLOCKWISE)).min_size(egui::vec2(132.0, 0.0))).clicked() {
                                    cms_step = Some((2, true));
                                }
                                if ui.add(egui::Button::new(ph::FILE_TEXT).frame(true)).on_hover_text("업데이트 명령어 보기").clicked() {
                                    cms_step = Some((2, false));
                                }
                            });
                        });
                        if c.kind == CmsKind::Rhymix {
                            ui.add_space(6.0);
                            let rx_src = rx_source.trim().to_string();
                            card(ui, |ui| {
                                ui.strong(format!("{}  Rhymix 모듈/레이아웃 업로드", ph::UPLOAD));
                                if rx_src.is_empty() {
                                    ui.label(egui::RichText::new("⚠ 설정 > 서버 SSH > 'Rhymix 소스'에 로컬 dev/rx 경로를 먼저 지정하세요.").weak().color(egui::Color32::from_rgb(200, 140, 50)));
                                } else {
                                    ui.label(egui::RichText::new(format!("소스: {rx_src}/modules · {rx_src}/layouts → 위에서 선택한 대상 서버의 사이트로 업로드(전체 교체).")).weak());
                                }
                                ui.add_space(4.0);
                                egui::Grid::new(("rxup", ui.next_auto_id())).num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                                    changed |= row_text_hint(ui, "모듈(쉼표)", &mut c.rx_modules, "예: mymod, point  (dev/rx/modules 하위 폴더명)");
                                    changed |= row_text_hint(ui, "레이아웃(쉼표)", &mut c.rx_layouts, "예: mylayout  (dev/rx/layouts 하위 폴더명)");
                                    ui.end_row();
                                });
                                ui.add_space(4.0);
                                ui.horizontal(|ui| {
                                    if ui.add_enabled(!running, btn_primary(format!("{}  업로드", ph::UPLOAD)).min_size(egui::vec2(132.0, 0.0))).clicked() {
                                        rx_upload = Some(true);
                                    }
                                    if ui.button(ph::FILE_TEXT).on_hover_text("업로드 명령어 보기").clicked() {
                                        rx_upload = Some(false);
                                    }
                                });
                            });
                        }
                        if c.kind == CmsKind::WordPress {
                            ui.add_space(6.0);
                            let wp_src = wp_source.trim().to_string();
                            card(ui, |ui| {
                                ui.strong(format!("{}  WordPress 플러그인 · 테마 (버전 비교 · 동기화)", ph::PUZZLE_PIECE));
                                // 플러그인 / 테마 전환 토글
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new("대상:").weak());
                                    ui.selectable_value(&mut wp_kind, ops::WpAssetKind::Plugin, "플러그인");
                                    ui.selectable_value(&mut wp_kind, ops::WpAssetKind::Theme, "테마");
                                });
                                let klbl = wp_kind.label();
                                let ksub = wp_kind.subdir();
                                // 캐시된 현재 종류의 행(있으면 즉시 표시 — 재조회 없음)
                                let wp_rows: &Vec<ops::WpPluginRow> = match wp_kind {
                                    ops::WpAssetKind::Plugin => &wp_rows_plugin,
                                    ops::WpAssetKind::Theme => &wp_rows_theme,
                                };
                                let cached = !wp_rows.is_empty();
                                if wp_src.is_empty() {
                                    ui.label(egui::RichText::new("⚠ 설정 > 서버 SSH > 'WordPress 소스'에 로컬 dev/wp 경로를 먼저 지정하세요.").weak().color(egui::Color32::from_rgb(200, 140, 50)));
                                } else {
                                    ui.label(egui::RichText::new(format!("소스: {wp_src}/wp-content/{ksub} → 위 대상 서버 사이트의 설치 버전과 비교(같은 서버, 경로만 있으면 자동 조회).")).weak());
                                }
                                ui.add_space(4.0);
                                ui.horizontal(|ui| {
                                    // 캐시가 있으면 '새로고침', 없으면 '스캔'. 캐시는 즉시 표시되고 이 버튼으로만 재조회.
                                    let btn_label = if cached { format!("{}  {klbl} 새로고침", ph::ARROWS_CLOCKWISE) } else { format!("{}  {klbl} 스캔(버전 비교)", ph::MAGNIFYING_GLASS) };
                                    if ui.add_enabled(!running && !wp_scanning && !wp_src.is_empty(), btn_primary(btn_label)).clicked() {
                                        wp_scan_req = true;
                                    }
                                    if wp_scanning { ui.spinner(); }
                                    ui.label(egui::RichText::new(&wp_scan_status).weak());
                                    if cached && !wp_scanning { ui.label(egui::RichText::new("· 캐시 표시중").weak()); }
                                });
                                if cached {
                                    ui.add_space(4.0);
                                    // 검색: 이름/slug 부분일치(대소문자 무시). 빈칸이면 전체.
                                    ui.horizontal(|ui| {
                                        ui.label(format!("{} 검색", ph::MAGNIFYING_GLASS));
                                        ui.add(egui::TextEdit::singleline(&mut wp_filter).hint_text("이름 또는 slug 일부").desired_width(200.0));
                                        if !wp_filter.trim().is_empty() && ui.small_button("✕").on_hover_text("검색 지우기").clicked() {
                                            wp_filter.clear();
                                        }
                                    });
                                    let fq = wp_filter.trim().to_lowercase();
                                    let matches = |r: &ops::WpPluginRow| fq.is_empty()
                                        || r.name.to_lowercase().contains(&fq)
                                        || r.slug.to_lowercase().contains(&fq);
                                    let shown = wp_rows.iter().filter(|r| matches(r)).count();
                                    ui.horizontal(|ui| {
                                        // 빠른 선택은 현재 검색결과(표시된 행)에만 적용 — "검색 후 전체 체크" 흐름.
                                        if ui.small_button("업데이트 가능만 선택").clicked() {
                                            wp_sel_local = wp_rows.iter().filter(|r| matches(r) && r.diff == ops::WpDiff::Update).map(|r| r.slug.clone()).collect();
                                        }
                                        if ui.small_button("로컬 있는 것 전체").on_hover_text("업로드용 (검색결과 내)").clicked() {
                                            wp_sel_local = wp_rows.iter().filter(|r| matches(r) && (r.local_size > 0 || !r.local_ver.is_empty())).map(|r| r.slug.clone()).collect();
                                        }
                                        if ui.small_button("원격 있는 것 전체").on_hover_text("내려받기용 (검색결과 내)").clicked() {
                                            wp_sel_local = wp_rows.iter().filter(|r| matches(r) && (r.remote_size > 0 || !r.remote_ver.is_empty())).map(|r| r.slug.clone()).collect();
                                        }
                                        if ui.small_button("검색결과 전체 선택").clicked() {
                                            for r in wp_rows.iter().filter(|r| matches(r)) { wp_sel_local.insert(r.slug.clone()); }
                                        }
                                        if ui.small_button("선택 해제").clicked() { wp_sel_local.clear(); }
                                        if !fq.is_empty() { ui.label(egui::RichText::new(format!("· {shown}/{}개 표시", wp_rows.len())).weak()); }
                                    });
                                    ui.add_space(2.0);
                                    egui::ScrollArea::horizontal().id_salt(("wpsc", ui.next_auto_id())).show(ui, |ui| {
                                    egui::Grid::new(("wpplugins", ui.next_auto_id())).striped(true).num_columns(9).spacing([12.0, 4.0]).show(ui, |ui| {
                                        for h in ["", klbl, "로컬ver", "원격ver", "상태", "로컬크기", "원격크기", "로컬수정", "원격수정"] {
                                            ui.label(egui::RichText::new(h).strong());
                                        }
                                        ui.end_row();
                                        let newer = egui::Color32::from_rgb(80, 190, 110);
                                        let older = egui::Color32::from_rgb(220, 160, 60);
                                        for r in wp_rows.iter().filter(|r| matches(r)) {
                                            // 로컬·원격 어느 쪽이든 존재하면 선택 가능(로컬만=업로드, 원격만=내려받기 대상).
                                            {
                                                let mut checked = wp_sel_local.contains(&r.slug);
                                                let tip = if r.local_size == 0 && r.local_ver.is_empty() { "원격에만 있음 → 내려받기 가능" }
                                                    else if r.remote_size == 0 && r.remote_ver.is_empty() { "로컬에만 있음 → 업로드 가능" }
                                                    else { "양쪽 존재 → 업로드/내려받기 가능" };
                                                if ui.checkbox(&mut checked, "").on_hover_text(tip).changed() {
                                                    if checked { wp_sel_local.insert(r.slug.clone()); } else { wp_sel_local.remove(&r.slug); }
                                                }
                                            }
                                            let nm = if r.active { format!("● {}", r.name) } else { r.name.clone() };
                                            ui.label(nm).on_hover_text(format!("{} (active={})", r.slug, r.active));
                                            ui.label(if r.local_ver.is_empty() { "-".to_string() } else { r.local_ver.clone() });
                                            ui.label(if r.remote_ver.is_empty() { "-".to_string() } else { r.remote_ver.clone() });
                                            let (txt, col) = wp_diff_label(r.diff);
                                            ui.label(egui::RichText::new(txt).color(col));
                                            ui.label(human_bytes(r.local_size));
                                            ui.label(human_bytes(r.remote_size));
                                            // 수정일: 더 최신 쪽을 색으로 강조 (로컬이 최신=초록, 원격이 최신=주황)
                                            let (lc, rc) = if r.local_mtime > 0 && r.remote_mtime > 0 {
                                                if r.local_mtime > r.remote_mtime { (Some(newer), None) }
                                                else if r.remote_mtime > r.local_mtime { (None, Some(older)) }
                                                else { (None, None) }
                                            } else { (None, None) };
                                            let mut lt = egui::RichText::new(short_date(r.local_mtime));
                                            if let Some(c) = lc { lt = lt.color(c); }
                                            ui.label(lt);
                                            let mut rt = egui::RichText::new(short_date(r.remote_mtime));
                                            if let Some(c) = rc { rt = rt.color(c); }
                                            ui.label(rt);
                                            ui.end_row();
                                        }
                                    });
                                    });
                                    ui.add_space(4.0);
                                    ui.horizontal(|ui| {
                                        let n = wp_sel_local.len();
                                        if ui.add_enabled(!running && n > 0, btn_primary(format!("{}  선택 {n}개 업로드(로컬→원격)", ph::UPLOAD)).min_size(egui::vec2(180.0, 0.0))).clicked() {
                                            wp_upload_req = Some(true);
                                        }
                                        if ui.button(ph::FILE_TEXT).on_hover_text("업로드 명령어 보기").clicked() {
                                            wp_upload_req = Some(false);
                                        }
                                        ui.separator();
                                        if ui.add_enabled(!running && n > 0, egui::Button::new(format!("{}  선택 {n}개 내려받기(원격→로컬)", ph::DOWNLOAD_SIMPLE)).min_size(egui::vec2(180.0, 0.0))).on_hover_text("원격 사이트의 선택 폴더를 로컬 소스로 복사(로컬 폴더 전체 교체)").clicked() {
                                            wp_download_req = Some(true);
                                        }
                                        if ui.button(ph::FILE_TEXT).on_hover_text("내려받기 명령어 보기").clicked() {
                                            wp_download_req = Some(false);
                                        }
                                    });
                                    ui.label(egui::RichText::new(format!("※ ● = 원격 활성 {klbl}. 업로드=선택 폴더를 사이트에 전체 교체 복사(활성 {klbl} 교체 시 잠깐 영향 가능). 내려받기=원격 폴더를 로컬 소스에 전체 교체 복사(로컬 원본 덮어씀).")).weak());
                                }
                            });
                        }
                    }
                    // ───────── eondcms 설치/업데이트 ─────────
                    Tab::Eond => {
                        let asis_ip = if domain.asis.ip.trim().is_empty() { "(IP 빔)".to_string() } else { domain.asis.ip.trim().to_string() };
                        let tobe_ip = if domain.tobe.ip.trim().is_empty() { "(IP 빔)".to_string() } else { domain.tobe.ip.trim().to_string() };
                        let e = &mut domain.eond;
                        card(ui, |ui| {
                            ui.strong("eondcms 설치 (HestiaCP)");
                            ui.horizontal(|ui| {
                                ui.label("대상 서버:");
                                changed |= ui.radio_value(&mut e.use_asis, true, format!("현재(ASIS) {asis_ip}")).changed();
                                changed |= ui.radio_value(&mut e.use_asis, false, format!("신규(TOBE) {tobe_ip}")).changed();
                            });
                            changed |= ui.checkbox(&mut e.sudo, "sudo 경유 (root 직접 로그인 불가 → tong 등으로 접속 후 sudo)")
                                .on_hover_text("켜짐: ssh <루트ID>@서버 후 sudo -S bash 로 실행. 끄면 root 직접 로그인 가정")
                                .changed();
                            egui::Grid::new(("eond", ui.next_auto_id())).num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                                changed |= row_text_hint(ui, "HestiaCP 유저", &mut e.hestia_user, "서버 계정명(영소문자) → /home/<유저>/");
                                changed |= row_secret_hint(ui, "유저 비번", &mut e.hestia_pass, show_pw, "신규 유저 생성 시에만 사용(있으면 무시)");
                                changed |= row_text_hint(ui, "유저 이메일", &mut e.hestia_email, "HestiaCP 유저 생성에 필요");
                                changed |= row_text_hint(ui, "패키지", &mut e.package, "HestiaCP 호스팅 패키지(보통 default)");
                                changed |= row_text_hint(ui, "포트", &mut e.port, "내부 uvicorn 포트, 인스턴스마다 고유 (예 8002)");
                                changed |= row_text_hint(ui, "DB 전체이름", &mut e.db_name, "입력 그대로 사용(.env). 예: omg_customer");
                                changed |= row_text_hint(ui, "DB 전체유저", &mut e.db_user, "입력 그대로 사용. 보통 DB이름과 동일");
                                changed |= row_secret_hint(ui, "DB 비번", &mut e.db_pass, show_pw, "DB 사용자 비밀번호");
                                changed |= row_text_hint(ui, "테이블접두어", &mut e.table_prefix, "Rhymix/XE 접두어(xe_/rx_). seed 적재 시 자동감지로 보정됨");
                                changed |= row_text_hint(ui, "관리자ID", &mut e.admin_user, "eondcms 관리자 로그인 ID(기본 admin)");
                                changed |= row_secret_hint(ui, "관리자 비번", &mut e.admin_pass, show_pw, "eondcms 관리자 비번(production 필수·강력하게)");
                                changed |= row_text_hint(ui, "코드 경로(dev)", &mut e.code_local, "이 PC의 eondcms pythonapp 경로(rsync 소스). web/build 빌드 필요");
                            });
                        });
                        ui.add_space(6.0);
                        card(ui, |ui| {
                            ui.strong("신규 설치  ·  순서: ① → ② → ③  ('루트로 실행' 필수)");
                            ui.add_space(4.0);
                            for (n, label) in [(1u8, "① 리소스 생성"), (2, "② 코드 업로드"), (3, "③ 설치 마무리")] {
                                ui.horizontal(|ui| {
                                    let b = egui::Button::new(label).min_size(egui::vec2(150.0, 0.0));
                                    let resp = if n == 3 {
                                        ui.add_enabled(!running, btn_primary(label).min_size(egui::vec2(150.0, 0.0)))
                                    } else {
                                        ui.add_enabled(!running, b)
                                    };
                                    if resp.clicked() {
                                        eond_step = Some((n, true));
                                    }
                                    if ui.button(ph::FILE_TEXT).on_hover_text("명령어 보기").clicked() {
                                        eond_step = Some((n, false));
                                    }
                                });
                            }
                        });
                        ui.add_space(6.0);
                        card(ui, |ui| {
                            ui.strong("코드 업데이트 (이미 설치된 인스턴스)");
                            ui.label(egui::RichText::new(format!("② 코드 업로드 → {} 업데이트 (v-*/SSL/nginx 생략, 재시작 포함)", ph::ARROWS_CLOCKWISE)).weak());
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                if ui.add_enabled(!running, egui::Button::new(format!("{}  업데이트", ph::ARROWS_CLOCKWISE)).min_size(egui::vec2(150.0, 0.0))).clicked() {
                                    eond_step = Some((4, true));
                                }
                                if ui.button(ph::FILE_TEXT).on_hover_text("명령어 보기").clicked() {
                                    eond_step = Some((4, false));
                                }
                            });
                        });
                    }
                    // ───────── 작업 기록 ─────────
                    Tab::History => {
                        let h = &domain.history;
                        card(ui, |ui| {
                            ui.strong("작업 요약");
                            ui.add_space(2.0);
                            let last_ok = |needles: &[&str]| -> Option<String> {
                                h.iter().rev().find(|e| e.ok && needles.iter().all(|n| e.title.contains(n))).map(|e| fmt_kst(e.at))
                            };
                            let upd_cnt = |needle: &str| h.iter().filter(|e| e.ok && e.title.contains(needle) && e.title.contains("업데이트")).count();
                            let row = |ui: &mut egui::Ui, label: &str, when: Option<String>, extra: String| {
                                ui.horizontal(|ui| {
                                    ui.allocate_ui_with_layout(egui::vec2(96.0, ui.spacing().interact_size.y), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                        ui.label(label);
                                    });
                                    match when {
                                        Some(d) => {
                                            ui.colored_label(egui::Color32::from_rgb(60, 190, 100), format!("✓ {d}"));
                                            if !extra.is_empty() { ui.weak(extra); }
                                        }
                                        None => { ui.weak("— 안 함"); }
                                    }
                                });
                            };
                            let upd = |n: usize| if n > 0 { format!("· 업데이트 {n}회") } else { String::new() };
                            let mig = h.iter().rev().find(|e| e.ok && e.title.contains("이전")).map(|e| fmt_kst(e.at));
                            row(ui, "사이트 이전", mig, String::new());
                            row(ui, "eondcms", last_ok(&["eondcms", "마무리"]), upd(upd_cnt("eondcms")));
                            row(ui, "WordPress", last_ok(&["WordPress 설치"]), upd(upd_cnt("WordPress")));
                            row(ui, "Rhymix", last_ok(&["Rhymix 설치"]), upd(upd_cnt("Rhymix")));
                            row(ui, "그누보드", last_ok(&["그누보드 설치"]), upd(upd_cnt("그누보드")));
                        });
                        ui.add_space(6.0);
                        card(ui, |ui| {
                            ui.strong(format!("전체 기록 ({}건)", h.len()));
                            ui.add_space(2.0);
                            if h.is_empty() {
                                ui.weak("아직 작업 기록이 없습니다. (백업·이전·설치·업데이트 완료 시 기록됨)");
                            } else {
                                for e in h.iter().rev() {
                                    ui.horizontal(|ui| {
                                        ui.add(egui::Label::new(egui::RichText::new(fmt_kst(e.at)).monospace().weak()));
                                        if e.ok {
                                            ui.colored_label(egui::Color32::from_rgb(60, 190, 100), "✓");
                                        } else {
                                            ui.colored_label(egui::Color32::from_rgb(230, 95, 95), "✗");
                                        }
                                        ui.label(&e.title);
                                    });
                                }
                            }
                        });
                    }
                }
              });
            });

            // CMS/eondcms 설치 탭 "진입" 시: 루트 권한이 필수 → 자동 체크.
            // (탭 클릭뿐 아니라 앱 복원·도메인 이동으로 그 탭에 처음 들어올 때도 동작하도록 직전 탭을 추적)
            if self.prev_domain_tab != Some(tab) {
                if matches!(tab, Tab::Eond | Tab::Cms) {
                    self.use_root = true; // 진입 시 자동 활성화
                } else if matches!(self.prev_domain_tab, Some(Tab::Eond) | Some(Tab::Cms)) {
                    self.use_root = false; // 설치 탭에서 다른 탭으로 나갈 때만 해제(그 외 탭은 원래대로 유지)
                }
                self.prev_domain_tab = Some(tab);
            }
            self.tab = tab;

            if delete_domain {
                // 즉시 삭제하지 않고 확인 모달 → 휴지통 이동
                self.pending_delete = Some(DelTarget::Domain(ci, di));
            } else if changed {
                self.dirty = true;
                self.last_edit = ctx.input(|i| i.time);
            }

            // 명령어 보기: 실행하지 않고 명령만 생성해 모달에 표시
            if let Some((kind, cn, dn, a, t)) = view_request {
                self.cmd_view = Some(match ops::build(kind, &cn, &dn, &a, &t, None, self.use_root) {
                    Ok(job) => CmdView { title: job.title.clone(), command: render_command(&job, show_pw) },
                    Err(e) => CmdView { title: "명령어 생성 불가".into(), command: e },
                });
            }

            // 드라이런 검토: 실행 없이 입력 요약 + 작업 준비상태 리포트
            if dryrun {
                let dom = &self.store.customers[ci].domains[di];
                let cn = customer_name.clone();
                let report = build_dryrun(dom, &cn, self.use_root);
                self.cmd_view = Some(CmdView { title: format!("드라이런 검토: {}", dom.name), command: report });
            }

            // eondcms 설치 단계 처리 (별도 빌더, EondInstall 사용)
            if let Some((step, run)) = eond_step {
                let dom = &self.store.customers[ci].domains[di];
                let eond = dom.eond.clone();
                let server = if eond.use_asis { dom.asis.clone() } else { dom.tobe.clone() };
                let dn = dom.name.clone();
                let built = match step {
                    1 => ops::build_eondcms_resources(&server, &eond, &dn, self.use_root),
                    2 => ops::build_eondcms_upload(&server, &eond, &dn, self.use_root),
                    3 => ops::build_eondcms_finalize(&server, &eond, &dn, self.use_root),
                    _ => ops::build_eondcms_update(&server, &eond, &dn, self.use_root),
                };
                match built {
                    Ok(job) => {
                        if run {
                            self.eond_confirm = Some(job);
                        } else {
                            self.cmd_view = Some(CmdView {
                                title: job.title.clone(),
                                command: render_command(&job, show_pw),
                            });
                        }
                    }
                    Err(e) => {
                        self.log.push(format!("eondcms: {e}"));
                        self.status = format!("오류: {e}");
                        self.last_ok = Some(false);
                    }
                }
            }

            // CMS(WordPress 등) 설치/업데이트 처리
            if let Some((step, run)) = cms_step {
                let dom = &self.store.customers[ci].domains[di];
                let mut c = dom.cms_install.clone();
                let server = if c.use_asis { dom.asis.clone() } else { dom.tobe.clone() };
                // 정보 탭을 단일 출처로 사용: HestiaCP 유저 = FTP 계정, DB = 선택 서버, 관리자 = ④ CMS 접속정보
                c.hestia_user = server.ftp_id.clone();
                c.hestia_pass = server.ftp_pw.clone();
                c.db_name = server.db_name.clone();
                c.db_user = server.db_id.clone();
                c.db_pass = server.db_pw.clone();
                c.admin_user = dom.cms.id.clone();
                c.admin_pass = dom.cms.pw.clone();
                // vhost 유저(=HestiaCP 계정)가 비면 고객명으로 폴백 (고객명 == HestiaCP 유저)
                if c.hestia_user.trim().is_empty() {
                    c.hestia_user = customer_name.clone();
                }
                let dn = dom.name.clone();
                let built = if step == 1 {
                    ops::build_cms_install(&server, &c, &dn, self.use_root)
                } else {
                    ops::build_cms_update(&server, &c, &self.store.settings, &dn, self.use_root)
                };
                match built {
                    Ok(job) => {
                        if run {
                            self.eond_confirm = Some(job);
                        } else {
                            self.cmd_view = Some(CmdView { title: job.title.clone(), command: render_command(&job, show_pw) });
                        }
                    }
                    Err(e) => {
                        self.log.push(format!("CMS: {e}"));
                        self.status = format!("오류: {e}");
                        self.last_ok = Some(false);
                    }
                }
            }

            // Rhymix 모듈/레이아웃 업로드 처리
            if let Some(run) = rx_upload {
                let dom = &self.store.customers[ci].domains[di];
                let mut c = dom.cms_install.clone();
                let server = if c.use_asis { dom.asis.clone() } else { dom.tobe.clone() };
                c.hestia_user = server.ftp_id.clone();
                c.hestia_pass = server.ftp_pw.clone();
                if c.hestia_user.trim().is_empty() { c.hestia_user = customer_name.clone(); }
                let modules: Vec<String> = c.rx_modules.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                let layouts: Vec<String> = c.rx_layouts.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                let dn = dom.name.clone();
                match ops::build_rx_upload(&server, &c, &self.store.settings.rx_source_local, &modules, &layouts, &dn, self.use_root) {
                    Ok(job) => {
                        if run {
                            self.eond_confirm = Some(job);
                        } else {
                            self.cmd_view = Some(CmdView { title: job.title.clone(), command: render_command(&job, show_pw) });
                        }
                    }
                    Err(e) => {
                        self.log.push(format!("Rhymix 업로드: {e}"));
                        self.status = format!("오류: {e}");
                        self.last_ok = Some(false);
                    }
                }
            }

            // WordPress 플러그인/테마: 선택·종류 반영 + 스캔/업로드 트리거 처리
            self.wp_sel = wp_sel_local;
            self.wp_kind = wp_kind;
            self.wp_filter = wp_filter;
            if wp_scan_req {
                self.scan_wp_plugins(ci, di, ctx);
            }
            if let Some(run) = wp_upload_req {
                let dom = &self.store.customers[ci].domains[di];
                let mut c = dom.cms_install.clone();
                let server = if c.use_asis { dom.asis.clone() } else { dom.tobe.clone() };
                c.hestia_user = server.ftp_id.clone();
                c.hestia_pass = server.ftp_pw.clone();
                if c.hestia_user.trim().is_empty() { c.hestia_user = customer_name.clone(); }
                let items: Vec<String> = self.wp_sel.iter().cloned().collect();
                let dn = dom.name.clone();
                match ops::build_wp_asset_upload(&server, &c, &self.store.settings.wp_source_local, &items, &dn, self.use_root, wp_kind) {
                    Ok(job) => {
                        if run {
                            self.eond_confirm = Some(job);
                        } else {
                            self.cmd_view = Some(CmdView { title: job.title.clone(), command: render_command(&job, show_pw) });
                        }
                    }
                    Err(e) => {
                        self.log.push(format!("WP {} 업로드: {e}", wp_kind.label()));
                        self.status = format!("오류: {e}");
                        self.last_ok = Some(false);
                    }
                }
            }
            if let Some(run) = wp_download_req {
                let dom = &self.store.customers[ci].domains[di];
                let mut c = dom.cms_install.clone();
                let server = if c.use_asis { dom.asis.clone() } else { dom.tobe.clone() };
                c.hestia_user = server.ftp_id.clone();
                c.hestia_pass = server.ftp_pw.clone();
                if c.hestia_user.trim().is_empty() { c.hestia_user = customer_name.clone(); }
                let items: Vec<String> = self.wp_sel.iter().cloned().collect();
                let dn = dom.name.clone();
                match ops::build_wp_asset_download(&server, &c, &self.store.settings.wp_source_local, &items, &dn, self.use_root, wp_kind) {
                    Ok(job) => {
                        if run {
                            self.eond_confirm = Some(job);
                        } else {
                            self.cmd_view = Some(CmdView { title: job.title.clone(), command: render_command(&job, show_pw) });
                        }
                    }
                    Err(e) => {
                        self.log.push(format!("WP {} 내려받기: {e}", wp_kind.label()));
                        self.status = format!("오류: {e}");
                        self.last_ok = Some(false);
                    }
                }
            }

            // 신규 사이트를 덮어쓰는 작업(복원/마이그레이션)은 확인 모달, 백업은 즉시 실행
            if let Some(action) = request {
                match action.0 {
                    Req::Op(OpKind::DbRestore)
                    | Req::Op(OpKind::FileRestore)
                    | Req::Op(OpKind::DbDirect)
                    | Req::Op(OpKind::FileDirect)
                    | Req::Op(OpKind::FixHtaccessAsis)
                    | Req::Op(OpKind::FixHtaccessTobe)
                    | Req::Op(OpKind::SetDbAsis)
                    | Req::Op(OpKind::SetDbTobe)
                    | Req::Migrate(_) => self.confirm = Some(action),
                    _ => self.start_action(action, ctx),
                }
            }
        });
    }

    fn confirm_modal(&mut self, ctx: &egui::Context) {
        let Some(action) = self.confirm.clone() else { return };
        let (req, _cn, dn, _asis, tobe) = &action;
        let (title, target) = match req {
            Req::Op(OpKind::DbRestore) => ("DB 복원", format!("신규 DB '{}' @ {}", tobe.db_name, tobe.ip)),
            Req::Op(OpKind::FileRestore) => ("파일 복원", format!("신규 {} @ {}", tobe.path, tobe.ip)),
            Req::Migrate(MigrateKind::Full) => (
                "전체 이전",
                format!("신규 사이트 전체 (DB '{}' / {}) @ {}", tobe.db_name, tobe.path, tobe.ip),
            ),
            Req::Migrate(MigrateKind::FilesOnly) => ("파일만 이전", format!("신규 {} @ {}", tobe.path, tobe.ip)),
            Req::Migrate(MigrateKind::DbOnly) => ("디비만 이전", format!("신규 DB '{}' @ {}", tobe.db_name, tobe.ip)),
            Req::Op(OpKind::FixHtaccessAsis) => ("htaccess 수정", format!("현재 사이트 .htaccess @ {}", _asis.ip)),
            Req::Op(OpKind::FixHtaccessTobe) => ("htaccess 수정", format!("신규 사이트 .htaccess @ {}", tobe.ip)),
            Req::Op(OpKind::SetDbAsis) => ("DB정보 반영", format!("현재 설정파일 DB → '{}' @ {}", _asis.db_name, _asis.ip)),
            Req::Op(OpKind::SetDbTobe) => ("DB정보 반영", format!("신규 설정파일 DB → '{}' @ {}", tobe.db_name, tobe.ip)),
            Req::Op(OpKind::DbDirect) => ("DB 직접 이전", format!("신규 DB '{}' @ {}", tobe.db_name, tobe.ip)),
            Req::Op(OpKind::FileDirect) => ("파일 직접 이전", format!("신규 {} @ {}", tobe.path, tobe.ip)),
            Req::Migrate(MigrateKind::Direct) => (
                "전체 직접 이전",
                format!("신규 전체 (DB '{}' / {}) @ {}", tobe.db_name, tobe.path, tobe.ip),
            ),
            _ => ("작업", String::new()),
        };
        egui::Window::new(format!("{title} 확인"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(format!("도메인: {dn}"));
                ui.colored_label(egui::Color32::from_rgb(220, 140, 60), format!("대상을 덮어씁니다: {target}"));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.add(btn_primary(format!("{}  실행", ph::PLAY))).clicked() {
                        if let Some(a) = self.confirm.take() {
                            self.start_action(a, ctx);
                        }
                    }
                    if ui.button("취소").clicked() {
                        self.confirm = None;
                    }
                });
            });
    }

    fn cmd_view_modal(&mut self, ctx: &egui::Context) {
        let Some(cv) = self.cmd_view.clone() else { return };
        let mut open = true;
        egui::Window::new(format!("명령어 — {}", cv.title))
            .collapsible(false)
            .resizable(true)
            .default_width(720.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label("실제 실행되는 쉘 명령입니다. 복사해서 직접 실행할 수도 있습니다.");
                ui.horizontal(|ui| {
                    if ui.button(format!("{}  전체 복사", ph::COPY)).clicked() {
                        ui.ctx().copy_text(cv.command.clone());
                    }
                    if ui.button("닫기").clicked() {
                        open = false;
                    }
                });
                ui.add_space(4.0);
                egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                    // 읽기용 코드 표시(선택/복사 가능). 편집 내용은 사용하지 않는다.
                    let mut text = cv.command.clone();
                    ui.add(
                        egui::TextEdit::multiline(&mut text)
                            .code_editor()
                            .desired_width(f32::INFINITY)
                            .desired_rows(10),
                    );
                });
            });
        if !open {
            self.cmd_view = None;
        }
    }

    fn eond_confirm_modal(&mut self, ctx: &egui::Context) {
        let (title, note) = match &self.eond_confirm {
            Some(j) => (j.title.clone(), j.note.clone()),
            None => return,
        };
        egui::Window::new("설치 작업 확인")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.strong(title);
                ui.colored_label(egui::Color32::from_rgb(220, 140, 60), &note);
                ui.colored_label(egui::Color32::from_rgb(220, 140, 60), "대상 서버 상태를 변경합니다.");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.add(btn_primary(format!("{}  실행", ph::PLAY))).clicked() {
                        if let Some(job) = self.eond_confirm.take() {
                            self.running = true;
                            self.last_ok = None;
                            self.status = "설치 작업 실행 중...".into();
                            self.running_job = self.current_domain_id().map(|id| (id, job.title.clone()));
                            let ctx2 = ctx.clone();
                            ops::spawn(job, self.tx.clone(), move || ctx2.request_repaint());
                        }
                    }
                    if ui.button("취소").clicked() {
                        self.eond_confirm = None;
                        self.pending_rescan.clear();
                    }
                });
            });
    }
}

/// WordPress 플러그인 버전차이 → (라벨, 색)
fn wp_diff_label(d: ops::WpDiff) -> (&'static str, egui::Color32) {
    match d {
        ops::WpDiff::Update => ("↑ 업데이트 가능", egui::Color32::from_rgb(80, 190, 110)),
        ops::WpDiff::LocalOnly => ("+ 신규(원격 없음)", egui::Color32::from_rgb(90, 160, 230)),
        ops::WpDiff::Newer => ("↓ 원격이 최신", egui::Color32::from_rgb(220, 160, 60)),
        ops::WpDiff::Same => ("= 동일", egui::Color32::from_gray(140)),
        ops::WpDiff::RemoteOnly => ("원격 전용(로컬 없음)", egui::Color32::from_gray(120)),
    }
}

/// 명령어 보기용 문자열 생성. SSH/FTP 비번은 SSHPASS 환경변수로 전달됨을 명시.
fn render_command(job: &ops::Job, show_pw: bool) -> String {
    let pw = if show_pw { job.sshpass.clone() } else { "********".to_string() };
    format!(
        "# bash 로 실행하세요 (dash 는 set -o pipefail 미지원)\n# SSH/FTP 비밀번호는 SSHPASS 환경변수로 전달됩니다 (argv 노출 방지)\nexport SSHPASS='{pw}'\n\n{}",
        job.script
    )
}

/// 드라이런 검토 리포트 (실행 없음): 사이트 입력 요약 + 작업별 준비 상태 + 형식 점검.
fn build_dryrun(d: &Domain, customer: &str, use_root: bool) -> String {
    let mut r = String::new();
    r.push_str(&format!("도메인: {}", d.name));
    if let Some(p) = puny_if_different(&d.name) {
        r.push_str(&format!("   (퓨니코드: {p})"));
    }
    r.push_str(&format!(
        "\n고객: {customer}    루트로 실행: {}\n\n",
        if use_root { "켜짐" } else { "꺼짐" }
    ));
    r.push_str(&site_summary("② 현재 사이트(ASIS)", &d.asis));
    r.push_str(&site_summary("③ 신규 사이트(TOBE)", &d.tobe));

    r.push_str("\n[작업 준비 상태] (실행하지 않음)\n");
    for (name, kind) in [
        ("DB 백업(현재→로컬)", OpKind::DbBackup),
        ("파일 백업(현재→로컬)", OpKind::FileBackup),
        ("DB 복원(로컬→신규)", OpKind::DbRestore),
        ("파일 복원(로컬→신규)", OpKind::FileRestore),
        ("DB 직접(현재→신규)", OpKind::DbDirect),
        ("파일 직접(현재→신규)", OpKind::FileDirect),
    ] {
        match ops::build(kind, customer, &d.name, &d.asis, &d.tobe, None, use_root) {
            Ok(_) => r.push_str(&format!("  OK  {name}: 준비됨\n")),
            Err(e) => r.push_str(&format!("  !!  {name}: {e}\n")),
        }
    }

    r.push_str("\n[eondcms 설치 준비]\n");
    let server = if d.eond.use_asis { &d.asis } else { &d.tobe };
    let er = ops::build_eondcms_resources(server, &d.eond, &d.name, use_root);
    let eu = ops::build_eondcms_upload(server, &d.eond, &d.name, use_root);
    let ef = ops::build_eondcms_finalize(server, &d.eond, &d.name, use_root);
    for (name, res) in [("① 리소스", er), ("② 코드 업로드", eu), ("③ 설치 마무리", ef)] {
        match res {
            Ok(_) => r.push_str(&format!("  OK  {name}: 준비됨\n")),
            Err(e) => r.push_str(&format!("  !!  {name}: {e}\n")),
        }
    }

    r.push_str("\n[형식 점검]\n");
    let mut warn = Vec::new();
    check_host(&d.asis.ip, "현재 IP", &mut warn);
    check_host(&d.tobe.ip, "신규 IP", &mut warn);
    if !d.eond.port.trim().is_empty() && d.eond.port.trim().parse::<u32>().is_err() {
        warn.push(format!("eondcms 포트가 숫자가 아님: '{}'", d.eond.port.trim()));
    }
    if warn.is_empty() {
        r.push_str("  특이사항 없음\n");
    } else {
        for w in warn {
            r.push_str(&format!("  !!  {w}\n"));
        }
    }
    r.push_str("\n※ 비밀번호는 ●●●● 로 마스킹됨. 실제 연결 검증은 🔌 접속 테스트 사용.");
    r
}

fn site_summary(label: &str, s: &Site) -> String {
    let v = |x: &str| if x.trim().is_empty() { "(빔)".to_string() } else { x.trim().to_string() };
    let pw = |x: &str| if x.is_empty() { "(빔)" } else { "●●●●" };
    format!(
        "[{label}]\n  IP: {}   DNS-A: {}\n  FTP: id={} pw={}    루트: id={} pw={}\n  DB: id={} pw={} name={} host={} port={}\n  경로: {}\n",
        v(&s.ip), v(&s.dns_a),
        v(&s.ftp_id), pw(&s.ftp_pw), v(&s.root_id), pw(&s.root_pw),
        v(&s.db_id), pw(&s.db_pw), v(&s.db_name), v(&s.db_host), v(&s.db_port),
        v(&s.path),
    )
}

fn check_host(ip: &str, label: &str, warn: &mut Vec<String>) {
    let t = ip.trim();
    if t.is_empty() {
        return;
    }
    if t.contains(' ') || t.starts_with("http") || t.contains('/') {
        warn.push(format!("{label} 형식 의심: '{t}'"));
    }
}

/// 값이 비어있지 않을 때 클립보드 복사 버튼
fn copy_btn(ui: &mut egui::Ui, value: &str) {
    let enabled = !value.trim().is_empty();
    if ui
        .add_enabled(enabled, egui::Button::new("복사").small())
        .on_hover_text("클립보드로 복사")
        .clicked()
    {
        ui.ctx().copy_text(value.to_owned());
    }
}

/// 비밀번호 입력칸 + 개별 눈(보기) 토글. show_global 이 켜져 있으면 항상 표시.
fn secret_input(ui: &mut egui::Ui, value: &mut String, show_global: bool, width: f32) -> bool {
    let key = egui::Id::new(("pwshow", value as *const String as usize));
    let mut local = ui.data_mut(|d| d.get_temp::<bool>(key).unwrap_or(false));
    let visible = show_global || local;
    let changed = ui.add(egui::TextEdit::singleline(value).password(!visible).desired_width(width).margin(FIELD_MARGIN)).changed();
    let icon = if visible { "🙈" } else { "👁" };
    if ui.add(egui::Button::new(icon).small()).on_hover_text("비밀번호 보기/숨기기").clicked() {
        local = !visible;
        ui.data_mut(|d| d.insert_temp(key, local));
    }
    changed
}

/// ① 도메인 접속정보 폼
fn access_editor(ui: &mut egui::Ui, a: &mut DomainAccess, show_pw: bool) -> bool {
    let mut changed = false;
    egui::Grid::new(("access", ui.next_auto_id())).num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
        changed |= row_text(ui, "관리 URL", &mut a.url);
        changed |= row_text(ui, "아이디", &mut a.id);
        changed |= row_secret(ui, "비밀번호", &mut a.pw, show_pw);
        changed |= row_domain(ui, "도메인", &mut a.domain);
        grid_label(ui, "네임서버");
        changed |= ui.add(egui::TextEdit::multiline(&mut a.nameservers).desired_rows(2)).changed();
        ui.end_row();
    });
    changed
}

/// ②③ 사이트 액션 버튼 클릭 신호 묶음
#[derive(Default)]
struct SiteActions {
    cert: bool,
    verify: bool,
    fix_htaccess: bool,
    set_db: bool,
}

/// 사이트 접속정보 입력 폼 (정보 탭). 변경 여부 반환.
fn site_fields(ui: &mut egui::Ui, s: &mut Site, show_pw: bool) -> bool {
    let mut changed = false;
    egui::Grid::new(("site", ui.next_auto_id())).num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
        changed |= row_text(ui, "IP", &mut s.ip);
        changed |= row_text(ui, "DNS A 호스트값", &mut s.dns_a);
        changed |= row_text(ui, "FTP 아이디", &mut s.ftp_id);
        changed |= row_secret(ui, "FTP 비번", &mut s.ftp_pw, show_pw);
        changed |= row_text(ui, "서버루트 ID", &mut s.root_id);
        changed |= row_secret(ui, "서버루트 PW", &mut s.root_pw, show_pw);
        changed |= row_text(ui, "DB 아이디", &mut s.db_id);
        changed |= row_secret(ui, "DB 비번", &mut s.db_pw, show_pw);
        changed |= row_text(ui, "DB 이름", &mut s.db_name);
        changed |= row_text(ui, "경로(path)", &mut s.path);
    });
    egui::CollapsingHeader::new("고급 (포트/DB호스트)").id_salt(ui.next_auto_id()).show(ui, |ui| {
        egui::Grid::new(("siteadv", ui.next_auto_id())).num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
            changed |= row_text(ui, "SSH 포트(기본22)", &mut s.ssh_port);
            changed |= row_text(ui, "DB 호스트(기본localhost)", &mut s.db_host);
            changed |= row_text(ui, "DB 포트(기본3306)", &mut s.db_port);
        });
    });
    changed
}

/// 사이트 진단/수정 액션 버튼 (진단 탭).
fn site_actions(ui: &mut egui::Ui, enabled: bool) -> SiteActions {
    let mut act = SiteActions::default();
    ui.horizontal_wrapped(|ui| {
        if ui.add_enabled(enabled, egui::Button::new(format!("{}  인증서 확인", ph::LOCK_KEY)))
            .on_hover_text("IP:443 에 SNI=도메인으로 접속해 SSL 인증서 설치/유효 확인").clicked() { act.cert = true; }
        if ui.add_enabled(enabled, egui::Button::new(format!("{}  사이트 점검", ph::HEARTBEAT)))
            .on_hover_text("SSH 로 PHP 버전/웹루트 권한/에러로그 확인 (500 등 진단)").clicked() { act.verify = true; }
        if ui.add_enabled(enabled, egui::Button::new(format!("{}  htaccess 수정", ph::WRENCH)))
            .on_hover_text("PHP-FPM 서버에서 500 유발하는 .htaccess php_flag/php_value 줄을 백업 후 주석처리").clicked() { act.fix_htaccess = true; }
        if ui.add_enabled(enabled, egui::Button::new(format!("{}  DB정보 반영", ph::DATABASE)))
            .on_hover_text("설정파일(wp-config 등) DB접속정보를 이 사이트의 DB칸 값으로 교체 (백업 후)").clicked() { act.set_db = true; }
    });
    act
}

/// ④ CMS 접속정보 폼
fn cms_editor(ui: &mut egui::Ui, c: &mut CmsAccess, show_pw: bool) -> bool {
    let mut changed = false;
    egui::Grid::new(("cms", ui.next_auto_id())).num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
        changed |= row_text(ui, "관리자 URL", &mut c.url);
        changed |= row_text(ui, "아이디", &mut c.id);
        changed |= row_secret(ui, "비밀번호", &mut c.pw, show_pw);
    });
    changed
}

/// 로그 줄이 성공/실패를 나타내는지 판별 (색상용)
fn is_success_line(l: &str) -> bool {
    l.contains('✓') || l.contains("접속 성공") || l.contains("설치됨") || l.contains("[자동입력]")
}
fn is_error_line(l: &str) -> bool {
    l.contains('✗')
        || l.contains("실패")
        || l.contains("ERROR")
        || l.contains("Error")
        || l.contains("error")
        || l.contains("denied")
        || l.contains("not found")
        || l.contains("없음")
        || l.contains("오류")
        || l.contains("Invalid")
}

/// 한글 도메인 → 퓨니코드(xn--) 변환. 실패 시 None.
fn to_punycode(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    idna::domain_to_ascii(t).ok()
}

/// 입력과 퓨니코드 결과가 다를 때만(=실제 한글/IDN 도메인일 때) 퓨니코드 반환.
fn puny_if_different(s: &str) -> Option<String> {
    let p = to_punycode(s)?;
    if p == s.trim() {
        None
    } else {
        Some(p)
    }
}

/// 도메인 입력 행: 한글 도메인(편집)+복사 + 퓨니코드(읽기전용, 복사 가능) 둘 다 표시
fn row_domain(ui: &mut egui::Ui, label: &str, value: &mut String) -> bool {
    grid_label(ui, label);
    let mut changed = false;
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            changed |= ui.add(egui::TextEdit::singleline(value).desired_width(180.0).margin(FIELD_MARGIN)).changed();
            copy_btn(ui, value);
        });
        if let Some(p) = puny_if_different(value) {
            ui.horizontal(|ui| {
                ui.weak("퓨니코드:");
                ui.add(egui::Label::new(egui::RichText::new(&p).monospace()).selectable(true))
                    .on_hover_text("클릭하여 드래그 복사");
                copy_btn(ui, &p);
            });
        }
    });
    ui.end_row();
    changed
}

fn row_text(ui: &mut egui::Ui, label: &str, value: &mut String) -> bool {
    grid_label(ui, label);
    let mut changed = false;
    ui.horizontal(|ui| {
        changed |= ui.add(egui::TextEdit::singleline(value).desired_width(180.0).margin(FIELD_MARGIN)).changed();
        copy_btn(ui, value);
    });
    ui.end_row();
    changed
}

/// 라벨 옆에 설명(회색)을 함께 보여주는 텍스트 행
fn row_text_hint(ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str) -> bool {
    grid_label(ui, label);
    let mut changed = false;
    ui.horizontal(|ui| {
        changed |= ui.add(egui::TextEdit::singleline(value).desired_width(160.0).margin(FIELD_MARGIN)).changed();
        copy_btn(ui, value);
        ui.weak(hint);
    });
    ui.end_row();
    changed
}

fn row_secret_hint(ui: &mut egui::Ui, label: &str, value: &mut String, show: bool, hint: &str) -> bool {
    grid_label(ui, label);
    let mut changed = false;
    ui.horizontal(|ui| {
        changed |= secret_input(ui, value, show, 160.0);
        copy_btn(ui, value);
        ui.weak(hint);
    });
    ui.end_row();
    changed
}

fn row_secret(ui: &mut egui::Ui, label: &str, value: &mut String, show: bool) -> bool {
    grid_label(ui, label);
    let mut changed = false;
    ui.horizontal(|ui| {
        changed |= secret_input(ui, value, show, 180.0);
        copy_btn(ui, value);
    });
    ui.end_row();
    changed
}
