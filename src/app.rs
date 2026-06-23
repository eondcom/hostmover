use crate::model::{CmsAccess, Customer, Domain, DomainAccess, Site, Store};
use crate::ops::{self, LogMsg, OpKind};
use crate::store;
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
    show_pw: bool,
    use_root: bool,

    new_customer: String,
    new_domain: String,

    log: Vec<String>,
    running: bool,
    confirm: Option<PendingAction>,
    cmd_view: Option<CmdView>,
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
            show_pw: false,
            use_root: false,
            new_customer: String::new(),
            new_domain: String::new(),
            log: Vec::new(),
            running: false,
            confirm: None,
            cmd_view: None,
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
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(80.0);
            ui.vertical_centered(|ui| {
                ui.heading("Hostmover");
                ui.label("호스팅 이전 백업/복원 관리");
                ui.add_space(20.0);

                if self.creating {
                    ui.label("새 마스터 패스워드를 설정하세요");
                    ui.add(egui::TextEdit::singleline(&mut self.password).password(true).hint_text("마스터 패스워드"));
                    ui.add(egui::TextEdit::singleline(&mut self.password_confirm).password(true).hint_text("패스워드 확인"));
                    if ui.button("생성").clicked() {
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
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.password).password(true).hint_text("마스터 패스워드"),
                    );
                    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button("잠금 해제").clicked() || enter {
                        match store::load(&self.password) {
                            Ok(s) => {
                                self.store = s;
                                self.master_pw = std::mem::take(&mut self.password);
                                self.locked = false;
                            }
                            Err(e) => self.auth_error = e,
                        }
                    }
                }

                if !self.auth_error.is_empty() {
                    ui.add_space(8.0);
                    ui.colored_label(egui::Color32::from_rgb(220, 80, 80), &self.auth_error);
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
        egui::SidePanel::left("tree").resizable(true).default_width(240.0).show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.new_customer).hint_text("새 고객명").desired_width(140.0));
                if ui.button("+ 고객").clicked() && !self.new_customer.trim().is_empty() {
                    let id = self.store.alloc_id();
                    self.store.customers.push(Customer {
                        id,
                        name: self.new_customer.trim().to_string(),
                        memo: String::new(),
                        domains: Vec::new(),
                    });
                    self.new_customer.clear();
                    self.dirty = true;
                    self.save();
                }
            });
            ui.separator();

            egui::ScrollArea::vertical().show(ui, |ui| {
                let mut delete_customer: Option<usize> = None;
                for ci in 0..self.store.customers.len() {
                    let cust_name = self.store.customers[ci].name.clone();
                    let header = egui::CollapsingHeader::new(format!("🏢 {cust_name}"))
                        .id_salt(("cust", self.store.customers[ci].id))
                        .default_open(true);
                    header.show(ui, |ui| {
                        let dlen = self.store.customers[ci].domains.len();
                        for di in 0..dlen {
                            let dname = self.store.customers[ci].domains[di].name.clone();
                            let selected = self.sel_customer == Some(ci) && self.sel_domain == Some(di);
                            if ui.selectable_label(selected, format!("🌐 {dname}")).clicked() {
                                self.sel_customer = Some(ci);
                                self.sel_domain = Some(di);
                            }
                        }
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut self.new_domain).hint_text("새 도메인").desired_width(120.0));
                            if ui.button("+ 도메인").clicked() && !self.new_domain.trim().is_empty() {
                                let id = self.store.alloc_id();
                                self.store.customers[ci].domains.push(Domain {
                                    id,
                                    name: self.new_domain.trim().to_string(),
                                    memo: String::new(),
                                    access: DomainAccess::default(),
                                    asis: Site::default(),
                                    tobe: Site::default(),
                                    cms: CmsAccess::default(),
                                });
                                self.new_domain.clear();
                                self.dirty = true;
                                self.save();
                            }
                        });
                        if ui.small_button("🗑 이 고객 삭제").clicked() {
                            delete_customer = Some(ci);
                        }
                    });
                }
                if let Some(ci) = delete_customer {
                    self.store.customers.remove(ci);
                    self.sel_customer = None;
                    self.sel_domain = None;
                    self.dirty = true;
                    self.save();
                }
            });
        });
    }

    fn bottom_log(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("log").resizable(true).default_height(170.0).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("작업 로그");
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
                (Some(c), Some(d)) if c < self.store.customers.len() && d < self.store.customers[c].domains.len() => (c, d),
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
            let mut delete_domain = false;

            let domain = &mut self.store.customers[ci].domains[di];
            let domain_name = domain.name.clone();

            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.heading(format!("🌐 {}", domain.name));
                    if let Some(p) = puny_if_different(&domain.name) {
                        ui.label("퓨니코드:");
                        ui.add(egui::Label::new(egui::RichText::new(&p).monospace()).selectable(true))
                            .on_hover_text("클릭하여 드래그 복사");
                    }
                    ui.label(format!("(고객: {customer_name})"));
                    if ui.button("🌐 DNS 조회").on_hover_text("whatsmydns.net 에서 A 레코드 전세계 전파 조회").clicked() {
                        let host = to_punycode(&domain.name).unwrap_or_else(|| domain.name.trim().to_string());
                        if !host.is_empty() {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(format!(
                                "https://www.whatsmydns.net/#A/{host}"
                            )));
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("🗑 도메인 삭제").clicked() {
                            delete_domain = true;
                        }
                    });
                });
                ui.horizontal(|ui| {
                    ui.label("메모");
                    changed |= ui.add(egui::TextEdit::singleline(&mut domain.memo).desired_width(400.0)).changed();
                });
                ui.separator();

                // ① 도메인 접속정보
                egui::CollapsingHeader::new("① 도메인 접속정보 (네임서버 변경은 수동)").default_open(true).show(ui, |ui| {
                    changed |= access_editor(ui, &mut domain.access, show_pw);
                });

                ui.add_space(6.0);
                // ② 현재 / ③ 신규 사이트
                ui.columns(2, |cols| {
                    cols[0].group(|ui| {
                        ui.strong("② 현재 사이트 (ASIS)");
                        let a = site_editor(ui, &mut domain.asis, show_pw, !running);
                        changed |= a.changed;
                        let mk = |k| Some((Req::Op(k), customer_name.clone(), domain_name.clone(), domain.asis.clone(), domain.tobe.clone()));
                        if a.test { request = mk(OpKind::TestAsis); }
                        if a.cert { request = mk(OpKind::CertAsis); }
                        if a.verify { request = mk(OpKind::VerifyAsis); }
                        if a.fix_htaccess { request = mk(OpKind::FixHtaccessAsis); }
                        if a.set_db { request = mk(OpKind::SetDbAsis); }
                    });
                    cols[1].group(|ui| {
                        ui.strong("③ 신규 사이트 (TOBE)");
                        let a = site_editor(ui, &mut domain.tobe, show_pw, !running);
                        changed |= a.changed;
                        let mk = |k| Some((Req::Op(k), customer_name.clone(), domain_name.clone(), domain.asis.clone(), domain.tobe.clone()));
                        if a.test { request = mk(OpKind::TestTobe); }
                        if a.cert { request = mk(OpKind::CertTobe); }
                        if a.verify { request = mk(OpKind::VerifyTobe); }
                        if a.fix_htaccess { request = mk(OpKind::FixHtaccessTobe); }
                        if a.set_db { request = mk(OpKind::SetDbTobe); }
                    });
                });

                ui.add_space(6.0);
                // ④ CMS 접속정보
                egui::CollapsingHeader::new("④ CMS 접속정보").show(ui, |ui| {
                    changed |= cms_editor(ui, &mut domain.cms, show_pw);
                });

                ui.add_space(8.0);
                let asis = domain.asis.clone();
                let tobe = domain.tobe.clone();
                ui.group(|ui| {
                    ui.strong("개별 작업");
                    ui.label("백업: 현재 사이트 → 로컬   |   복원: 로컬 → 신규 사이트");
                    ui.add_space(4.0);
                    for (label, kind) in [
                        ("⬇ DB 백업", OpKind::DbBackup),
                        ("⬇ 파일 백업", OpKind::FileBackup),
                        ("⬆ DB 복원", OpKind::DbRestore),
                        ("⬆ 파일 복원", OpKind::FileRestore),
                    ] {
                        ui.horizontal(|ui| {
                            if ui.add_enabled(!running, egui::Button::new(label).min_size(egui::vec2(120.0, 0.0))).clicked() {
                                request = Some((Req::Op(kind), customer_name.clone(), domain_name.clone(), asis.clone(), tobe.clone()));
                            }
                            if ui.button("📋 명령어").on_hover_text("실행하지 않고 명령어만 보기/복사").clicked() {
                                view_request = Some((kind, customer_name.clone(), domain_name.clone(), asis.clone(), tobe.clone()));
                            }
                        });
                    }
                });

                ui.add_space(6.0);
                ui.group(|ui| {
                    ui.strong("묶음 이전 (현재 → 신규)");
                    ui.label("전체 = DB백업→파일백업→DB복원→파일복원");
                    ui.label("파일만 = 파일백업→파일복원   |   디비만 = DB백업→DB복원");
                    ui.label("※ 디스크 공간이 부족하면 '파일만' 먼저 → 공간 확보 후 '디비만' 으로 나눠 진행");
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        let full = egui::Button::new("🚀 전체 이전").fill(egui::Color32::from_rgb(40, 110, 70));
                        if ui.add_enabled(!running, full).clicked() {
                            request = Some((Req::Migrate(MigrateKind::Full), customer_name.clone(), domain_name.clone(), asis.clone(), tobe.clone()));
                        }
                        if ui.add_enabled(!running, egui::Button::new("📁 파일만 이전")).clicked() {
                            request = Some((Req::Migrate(MigrateKind::FilesOnly), customer_name.clone(), domain_name.clone(), asis.clone(), tobe.clone()));
                        }
                        if ui.add_enabled(!running, egui::Button::new("🗄 디비만 이전")).clicked() {
                            request = Some((Req::Migrate(MigrateKind::DbOnly), customer_name.clone(), domain_name.clone(), asis.clone(), tobe.clone()));
                        }
                    });
                });

                ui.add_space(6.0);
                ui.group(|ui| {
                    ui.strong("⚡ 직접 이전 (현재 → 신규, 로컬 디스크 미사용)");
                    ui.label("로컬에 저장하지 않고 신규 서버로 바로 스트리밍합니다. 디스크 공간 부족 시 유용.");
                    ui.add_space(4.0);
                    for (label, kind) in [("⚡ DB 직접", OpKind::DbDirect), ("⚡ 파일 직접", OpKind::FileDirect)] {
                        ui.horizontal(|ui| {
                            if ui.add_enabled(!running, egui::Button::new(label).min_size(egui::vec2(120.0, 0.0))).clicked() {
                                request = Some((Req::Op(kind), customer_name.clone(), domain_name.clone(), asis.clone(), tobe.clone()));
                            }
                            if ui.button("📋 명령어").on_hover_text("실행하지 않고 명령어만 보기/복사").clicked() {
                                view_request = Some((kind, customer_name.clone(), domain_name.clone(), asis.clone(), tobe.clone()));
                            }
                        });
                    }
                    let all = egui::Button::new("⚡ 전체 직접 이전 (DB + 파일)")
                        .fill(egui::Color32::from_rgb(70, 90, 130));
                    if ui.add_enabled(!running, all).clicked() {
                        request = Some((Req::Migrate(MigrateKind::Direct), customer_name.clone(), domain_name.clone(), asis.clone(), tobe.clone()));
                    }
                });
            });

            if delete_domain {
                self.store.customers[ci].domains.remove(di);
                self.sel_domain = None;
                self.dirty = true;
                self.save();
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
                    if ui.button("⎘ 전체 복사").clicked() {
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
}

/// 명령어 보기용 문자열 생성. SSH/FTP 비번은 SSHPASS 환경변수로 전달됨을 명시.
fn render_command(job: &ops::Job, show_pw: bool) -> String {
    let pw = if show_pw { job.sshpass.clone() } else { "********".to_string() };
    format!(
        "# bash 로 실행하세요 (dash 는 set -o pipefail 미지원)\n# SSH/FTP 비밀번호는 SSHPASS 환경변수로 전달됩니다 (argv 노출 방지)\nexport SSHPASS='{pw}'\n\n{}",
        job.script
    )
}

/// 값이 비어있지 않을 때 클립보드 복사 버튼
fn copy_btn(ui: &mut egui::Ui, value: &str) {
    let enabled = !value.trim().is_empty();
    if ui
        .add_enabled(enabled, egui::Button::new("⎘").small())
        .on_hover_text("복사")
        .clicked()
    {
        ui.ctx().copy_text(value.to_owned());
    }
}

/// ① 도메인 접속정보 폼
fn access_editor(ui: &mut egui::Ui, a: &mut DomainAccess, show_pw: bool) -> bool {
    let mut changed = false;
    egui::Grid::new(("access", ui.next_auto_id())).num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
        changed |= row_text(ui, "관리 URL", &mut a.url);
        changed |= row_text(ui, "아이디", &mut a.id);
        changed |= row_secret(ui, "비밀번호", &mut a.pw, show_pw);
        changed |= row_domain(ui, "도메인", &mut a.domain);
        ui.label("네임서버");
        changed |= ui.add(egui::TextEdit::multiline(&mut a.nameservers).desired_rows(2)).changed();
        ui.end_row();
    });
    changed
}

/// ②③ 사이트 접속정보 폼. 반환: 변경/클릭 신호 묶음
#[derive(Default)]
struct SiteActions {
    changed: bool,
    test: bool,
    cert: bool,
    verify: bool,
    fix_htaccess: bool,
    set_db: bool,
}

fn site_editor(ui: &mut egui::Ui, s: &mut Site, show_pw: bool, enabled: bool) -> SiteActions {
    let mut act = SiteActions::default();
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(enabled, egui::Button::new("🔌 접속 테스트"))
            .on_hover_text("SSH 로그인 성공 여부 + 원격 도구(mysqldump/rsync/tar) 확인")
            .clicked()
        {
            act.test = true;
        }
        if ui
            .add_enabled(enabled, egui::Button::new("🔒 인증서 확인"))
            .on_hover_text("IP:443 에 SNI=도메인으로 접속해 SSL 인증서 설치/유효 확인")
            .clicked()
        {
            act.cert = true;
        }
        if ui
            .add_enabled(enabled, egui::Button::new("🩺 사이트 점검"))
            .on_hover_text("SSH 로 PHP 버전/웹루트 권한/에러로그 확인 (500 등 진단)")
            .clicked()
        {
            act.verify = true;
        }
        if ui
            .add_enabled(enabled, egui::Button::new("🔧 htaccess 수정"))
            .on_hover_text("PHP-FPM 서버에서 500 유발하는 .htaccess php_flag/php_value 줄을 백업 후 주석처리")
            .clicked()
        {
            act.fix_htaccess = true;
        }
        if ui
            .add_enabled(enabled, egui::Button::new("🛠 DB정보 반영"))
            .on_hover_text("설정파일(wp-config 등) DB접속정보를 이 사이트의 DB칸 값으로 교체 (백업 후)")
            .clicked()
        {
            act.set_db = true;
        }
    });
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
    act.changed = changed;
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
    ui.label(label);
    let mut changed = false;
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            changed |= ui.add(egui::TextEdit::singleline(value).desired_width(180.0)).changed();
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
    ui.label(label);
    let mut changed = false;
    ui.horizontal(|ui| {
        changed |= ui.add(egui::TextEdit::singleline(value).desired_width(180.0)).changed();
        copy_btn(ui, value);
    });
    ui.end_row();
    changed
}

fn row_secret(ui: &mut egui::Ui, label: &str, value: &mut String, show: bool) -> bool {
    ui.label(label);
    let mut changed = false;
    ui.horizontal(|ui| {
        changed |= ui.add(egui::TextEdit::singleline(value).password(!show).desired_width(180.0)).changed();
        copy_btn(ui, value);
    });
    ui.end_row();
    changed
}
