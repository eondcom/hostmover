use crate::model::{ActivityLog, CmsAccess, CmsKind, Customer, Domain, DomainAccess, Site, Store};
use crate::ops::{self, LogMsg, OpKind};
use crate::store;
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

    log: Vec<String>,
    running: bool,
    confirm: Option<PendingAction>,
    cmd_view: Option<CmdView>,
    eond_confirm: Option<ops::Job>,
    last_edit: f64,

    tx: Sender<LogMsg>,
    rx: Receiver<LogMsg>,
}

impl App {
    pub fn new() -> Self {
        let (tx, rx) = channel();
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
            log: Vec::new(),
            running: false,
            confirm: None,
            cmd_view: None,
            eond_confirm: None,
            last_edit: 0.0,
            tx,
            rx,
        }
    }

    fn save(&mut self) {
        match store::save(&self.master_pw, &self.store) {
            Ok(_) => {
                self.dirty = false;
                self.status = "저장 완료".into();
            }
            Err(e) => self.status = format!("저장 실패: {e}"),
        }
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
        self.bottom_log(ctx);
        self.central(ctx);

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
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Hostmover");
                ui.separator();
                if ui.button("💾 지금 저장").clicked() {
                    self.save();
                }
                ui.weak(if self.dirty { "자동저장 대기…" } else { "자동저장됨 ✓" });
                ui.separator();
                ui.checkbox(&mut self.show_pw, "비밀번호 표시");
                ui.checkbox(&mut self.use_root, "루트로 실행")
                    .on_hover_text("켜면 서버루트 계정(있으면)으로 SSH 접속. 명령어 보기에도 반영됨");
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
        for ci in 0..self.store.customers.len() {
            if self.store.customers[ci].deleted_at.is_some() {
                continue;
            }
            let cust_id = self.store.customers[ci].id;
            let name = self.store.customers[ci].name.clone();
            let collapsed = self.collapsed.contains(&cust_id);
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new(if collapsed { "▶" } else { "▼" }).small().frame(false)).clicked() {
                    if collapsed { self.collapsed.remove(&cust_id); } else { self.collapsed.insert(cust_id); }
                }
                ui.label(egui::RichText::new(format!("🏢 {name}")).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("🗑").on_hover_text("이 고객 삭제(휴지통)").clicked() {
                        self.pending_delete = Some(DelTarget::Customer(ci));
                    }
                    if ui.small_button("➕").on_hover_text("새 도메인 추가").clicked() {
                        if self.add_open.contains(&cust_id) { self.add_open.remove(&cust_id); } else { self.add_open.insert(cust_id); }
                    }
                });
            });
            if collapsed {
                continue;
            }
            ui.indent(("c", cust_id), |ui| {
                let dlen = self.store.customers[ci].domains.len();
                for di in 0..dlen {
                    if self.store.customers[ci].domains[di].deleted_at.is_some() {
                        continue;
                    }
                    let dname = self.store.customers[ci].domains[di].name.clone();
                    let selected = self.sel_customer == Some(ci) && self.sel_domain == Some(di);
                    if ui.selectable_label(selected, format!("🌐 {dname}")).clicked() {
                        select = Some((ci, di));
                    }
                }
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
                            self.store.customers[ci].domains.push(Domain {
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
                            self.dirty = true;
                            self.save();
                        }
                    });
                }
            });
        }
        if let Some((ci, di)) = select {
            self.sel_customer = Some(ci);
            self.sel_domain = Some(di);
        }
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
                    ui.label(format!("🏢 {name}"));
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
                            ui.label(format!("🌐 {dn}"));
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
                    if ui.button("휴지통으로 이동").clicked() {
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
        egui::TopBottomPanel::bottom("log").resizable(true).default_height(170.0).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("작업 로그");
                if ui.small_button("전체 복사").on_hover_text("작업 로그 전체를 클립보드로 복사").clicked() {
                    ui.ctx().copy_text(self.log.join("\n"));
                    self.status = "로그 복사됨".into();
                }
                if ui.small_button("지우기").clicked() {
                    self.log.clear();
                }
            });
            egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
                for line in &self.log {
                    let txt = egui::RichText::new(line).monospace();
                    if is_success_line(line) {
                        ui.label(txt.color(egui::Color32::from_rgb(60, 190, 100)).strong());
                    } else if is_error_line(line) {
                        ui.label(txt.color(egui::Color32::from_rgb(230, 95, 95)));
                    } else {
                        ui.label(txt);
                    }
                }
            });
        });
    }

    fn central(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
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
            let mut changed = false;
            let mut request: Option<PendingAction> = None;
            let mut view_request: Option<(OpKind, String, String, Site, Site)> = None;
            // eondcms 설치: (단계 1~3, 실행여부) — 실행이면 true, 명령어보기면 false
            let mut eond_step: Option<(u8, bool)> = None;
            // CMS 설치: (1=설치/2=업데이트, 실행여부)
            let mut cms_step: Option<(u8, bool)> = None;
            let mut dryrun = false;
            let mut delete_domain = false;

            let domain = &mut self.store.customers[ci].domains[di];
            let domain_name = domain.name.clone();
            // 도메인마다 위젯 ID를 분리해 편집 상태(커서/IME)가 도메인 간 공유되는 버그 방지
            let did = domain.id;

            let mut tab = self.tab;

            // ── 헤더 (항상 표시) ──
            ui.horizontal(|ui| {
                ui.heading(format!("🌐 {}", domain.name));
                if let Some(p) = puny_if_different(&domain.name) {
                    ui.add(egui::Label::new(egui::RichText::new(&p).monospace().weak()).selectable(true))
                        .on_hover_text("퓨니코드 (드래그 복사)");
                }
                ui.label(egui::RichText::new(format!("· {customer_name}")).weak());
                if ui.button("🌐 DNS").on_hover_text("whatsmydns.net 에서 A 레코드 전세계 전파 조회").clicked() {
                    let host = to_punycode(&domain.name).unwrap_or_else(|| domain.name.trim().to_string());
                    if !host.is_empty() {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(format!("https://www.whatsmydns.net/#A/{host}")));
                    }
                }
                if ui.button("🔎 드라이런").on_hover_text("실행하지 않고 입력값/작업 준비상태를 점검").clicked() {
                    dryrun = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("🗑 삭제").on_hover_text("이 도메인을 휴지통으로 (확인 후)").clicked() {
                        delete_domain = true;
                    }
                });
            });
            // ── 탭 바 ──
            ui.add_space(4.0);
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
            ui.separator();
            ui.add_space(2.0);

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
                            });
                            card(&mut cols[1], |ui| {
                                ui.strong("③ 신규 사이트 (TOBE)");
                                changed |= site_fields(ui, &mut domain.tobe, show_pw);
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
                                    if a.test { request = mk(OpKind::TestAsis); }
                                    if a.cert { request = mk(OpKind::CertAsis); }
                                    if a.verify { request = mk(OpKind::VerifyAsis); }
                                    if a.fix_htaccess { request = mk(OpKind::FixHtaccessAsis); }
                                    if a.set_db { request = mk(OpKind::SetDbAsis); }
                                });
                                card(&mut cols[1], |ui| {
                                    ui.strong("③ 신규 (TOBE)");
                                    let a = site_actions(ui, !running);
                                    let mk = |k| Some((Req::Op(k), customer_name.clone(), domain_name.clone(), domain.asis.clone(), domain.tobe.clone()));
                                    if a.test { request = mk(OpKind::TestTobe); }
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
                                    ("⬇ DB 백업", OpKind::DbBackup),
                                    ("⬇ 파일 백업", OpKind::FileBackup),
                                    ("⬆ DB 복원", OpKind::DbRestore),
                                    ("⬆ 파일 복원", OpKind::FileRestore),
                                ] {
                                    ui.horizontal(|ui| {
                                        if ui.add_enabled(!running, egui::Button::new(label).min_size(egui::vec2(118.0, 0.0))).clicked() {
                                            request = Some((Req::Op(kind), customer_name.clone(), domain_name.clone(), asis.clone(), tobe.clone()));
                                        }
                                        if ui.button("📋").on_hover_text("명령어만 보기/복사").clicked() {
                                            view_request = Some((kind, customer_name.clone(), domain_name.clone(), asis.clone(), tobe.clone()));
                                        }
                                    });
                                }
                            });
                            cols[1].vertical(|ui| {
                                card(ui, |ui| {
                                    ui.strong("묶음 이전 (현재 → 신규)");
                                    ui.label(egui::RichText::new("※ 공간 부족 시 '파일만' → 확보 후 '디비만'").weak());
                                    ui.add_space(4.0);
                                    ui.horizontal_wrapped(|ui| {
                                        let full = egui::Button::new("🚀 전체 이전").fill(egui::Color32::from_rgb(34, 110, 64));
                                        if ui.add_enabled(!running, full).clicked() {
                                            request = Some((Req::Migrate(MigrateKind::Full), customer_name.clone(), domain_name.clone(), asis.clone(), tobe.clone()));
                                        }
                                        if ui.add_enabled(!running, egui::Button::new("📁 파일만")).clicked() {
                                            request = Some((Req::Migrate(MigrateKind::FilesOnly), customer_name.clone(), domain_name.clone(), asis.clone(), tobe.clone()));
                                        }
                                        if ui.add_enabled(!running, egui::Button::new("🗄 디비만")).clicked() {
                                            request = Some((Req::Migrate(MigrateKind::DbOnly), customer_name.clone(), domain_name.clone(), asis.clone(), tobe.clone()));
                                        }
                                    });
                                });
                                ui.add_space(6.0);
                                card(ui, |ui| {
                                    ui.strong("⚡ 직접 이전 (디스크 미사용)");
                                    ui.label(egui::RichText::new("로컬 저장 없이 신규로 스트리밍").weak());
                                    ui.add_space(4.0);
                                    ui.horizontal_wrapped(|ui| {
                                        if ui.add_enabled(!running, egui::Button::new("⚡ DB 직접")).clicked() {
                                            request = Some((Req::Op(OpKind::DbDirect), customer_name.clone(), domain_name.clone(), asis.clone(), tobe.clone()));
                                        }
                                        if ui.add_enabled(!running, egui::Button::new("⚡ 파일 직접")).clicked() {
                                            request = Some((Req::Op(OpKind::FileDirect), customer_name.clone(), domain_name.clone(), asis.clone(), tobe.clone()));
                                        }
                                        let all = egui::Button::new("⚡ 전체 직접").fill(egui::Color32::from_rgb(46, 70, 120));
                                        if ui.add_enabled(!running, all).clicked() {
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
                                changed |= ui.radio_value(&mut c.use_asis, false, format!("신규(TOBE) {tobe_ip}")).changed();
                                changed |= ui.radio_value(&mut c.use_asis, true, format!("현재(ASIS) {asis_ip}")).changed();
                            });
                            changed |= ui.checkbox(&mut c.sudo, "sudo 경유 (root 직접 로그인 불가 → tong 등으로 접속 후 sudo)").changed();
                            egui::Grid::new(("cmsinstall", ui.next_auto_id())).num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                                changed |= row_text_hint(ui, "HestiaCP 유저", &mut c.hestia_user, "서버 계정명(영소문자) → /home/<유저>/");
                                changed |= row_secret_hint(ui, "유저 비번", &mut c.hestia_pass, show_pw, "신규 유저 생성 시에만(있으면 무시)");
                                changed |= row_text_hint(ui, "유저 이메일", &mut c.hestia_email, "HestiaCP 유저 생성에 필요");
                                changed |= row_text_hint(ui, "패키지", &mut c.package, "HestiaCP 패키지(보통 default)");
                                changed |= row_text_hint(ui, "DB 전체이름", &mut c.db_name, "예: wpuser_wp. 없으면 생성(접두어 자동 정리)");
                                changed |= row_text_hint(ui, "DB 전체유저", &mut c.db_user, "보통 DB이름과 동일");
                                changed |= row_secret_hint(ui, "DB 비번", &mut c.db_pass, show_pw, "DB 사용자 비밀번호");
                                changed |= row_text_hint(ui, "관리자ID", &mut c.admin_user, "관리자 로그인 ID(기본 admin)");
                                changed |= row_secret_hint(ui, "관리자 비번", &mut c.admin_pass, show_pw, "관리자 비밀번호");
                                changed |= row_text_hint(ui, "관리자 이메일", &mut c.admin_email, "WordPress 필수");
                                changed |= row_text_hint(ui, "사이트 제목", &mut c.site_title, "WordPress 사이트 제목(기본 My Site)");
                                changed |= row_text_hint(ui, "언어", &mut c.locale, "기본 ko_KR");
                                changed |= row_text_hint(ui, "버전", &mut c.version, "기본 latest (예: 6.5)");
                            });
                        });
                        ui.add_space(6.0);
                        card(ui, |ui| {
                            ui.strong(format!("{}  ('루트로 실행' 필수)", c.kind.label()));
                            let hint = match c.kind {
                                CmsKind::WordPress => "설치: wp-cli 코어+DB+관리자(완전 자동)  /  업데이트: 코어·플러그인·테마·언어",
                                CmsKind::Rhymix => "설치: git clone+DB+권한+SSL → 마지막에 브라우저 마법사  /  업데이트: git pull",
                                CmsKind::Gnuboard => "설치: git clone+DB+data권한+SSL → /install/ 마법사  /  업데이트: git pull",
                            };
                            ui.label(egui::RichText::new(hint).weak());
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                if ui.add_enabled(!running, egui::Button::new("설치").min_size(egui::vec2(140.0, 0.0))).clicked() {
                                    cms_step = Some((1, true));
                                }
                                if ui.button("📋").on_hover_text("설치 명령어 보기").clicked() {
                                    cms_step = Some((1, false));
                                }
                                let upd = egui::Button::new("🔄 업데이트").min_size(egui::vec2(140.0, 0.0)).fill(egui::Color32::from_rgb(46, 70, 120));
                                if ui.add_enabled(!running, upd).clicked() {
                                    cms_step = Some((2, true));
                                }
                                if ui.button("📋 ").on_hover_text("업데이트 명령어 보기").clicked() {
                                    cms_step = Some((2, false));
                                }
                            });
                        });
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
                                changed |= ui.radio_value(&mut e.use_asis, false, format!("신규(TOBE) {tobe_ip}")).changed();
                                changed |= ui.radio_value(&mut e.use_asis, true, format!("현재(ASIS) {asis_ip}")).changed();
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
                                    if ui.add_enabled(!running, egui::Button::new(label).min_size(egui::vec2(140.0, 0.0))).clicked() {
                                        eond_step = Some((n, true));
                                    }
                                    if ui.button("📋").on_hover_text("명령어 보기").clicked() {
                                        eond_step = Some((n, false));
                                    }
                                });
                            }
                        });
                        ui.add_space(6.0);
                        card(ui, |ui| {
                            ui.strong("코드 업데이트 (이미 설치된 인스턴스)");
                            ui.label(egui::RichText::new("② 코드 업로드 → 🔄 업데이트 (v-*/SSL/nginx 생략, 재시작 포함)").weak());
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                let btn = egui::Button::new("🔄 업데이트").min_size(egui::vec2(140.0, 0.0)).fill(egui::Color32::from_rgb(46, 70, 120));
                                if ui.add_enabled(!running, btn).clicked() {
                                    eond_step = Some((4, true));
                                }
                                if ui.button("📋").on_hover_text("명령어 보기").clicked() {
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

            // 탭 전환 시: CMS/eondcms 설치 탭은 루트 권한이 필수 → 자동 체크, 벗어나면 해제
            if tab != self.tab {
                self.use_root = matches!(tab, Tab::Eond | Tab::Cms);
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
                let c = dom.cms_install.clone();
                let server = if c.use_asis { dom.asis.clone() } else { dom.tobe.clone() };
                let dn = dom.name.clone();
                let built = if step == 1 {
                    ops::build_cms_install(&server, &c, &dn, self.use_root)
                } else {
                    ops::build_cms_update(&server, &c, &dn, self.use_root)
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
                    if ui.button("실행").clicked() {
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
                    if ui.button("전체 복사").clicked() {
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
                    if ui.button("실행").clicked() {
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
                    }
                });
            });
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
    test: bool,
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
        if ui.add_enabled(enabled, egui::Button::new("🔌 접속 테스트"))
            .on_hover_text("SSH 로그인 성공 여부 + 원격 도구(mysqldump/rsync/tar) 확인").clicked() { act.test = true; }
        if ui.add_enabled(enabled, egui::Button::new("🔒 인증서 확인"))
            .on_hover_text("IP:443 에 SNI=도메인으로 접속해 SSL 인증서 설치/유효 확인").clicked() { act.cert = true; }
        if ui.add_enabled(enabled, egui::Button::new("🩺 사이트 점검"))
            .on_hover_text("SSH 로 PHP 버전/웹루트 권한/에러로그 확인 (500 등 진단)").clicked() { act.verify = true; }
        if ui.add_enabled(enabled, egui::Button::new("🔧 htaccess 수정"))
            .on_hover_text("PHP-FPM 서버에서 500 유발하는 .htaccess php_flag/php_value 줄을 백업 후 주석처리").clicked() { act.fix_htaccess = true; }
        if ui.add_enabled(enabled, egui::Button::new("🛠 DB정보 반영"))
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
