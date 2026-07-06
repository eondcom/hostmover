use serde::{Deserialize, Serialize};

/// 최상위 저장소. 암호화되어 store.enc 에 직렬화된다.
#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Store {
    #[serde(default)]
    pub next_id: u64,
    #[serde(default)]
    pub customers: Vec<Customer>,
    #[serde(default)]
    pub settings: Settings,
    /// 마지막 전체 사이트 스캔 캐시 (다음 실행 시 즉시 표시)
    #[serde(default)]
    pub scan_cache: Vec<CachedSite>,
    /// 스캔 캐시 시각 (unix초)
    #[serde(default)]
    pub scan_cache_at: i64,
    /// 마지막 화면 상태 (재시작 시 복원)
    #[serde(default)]
    pub ui: UiState,
}

/// 재시작 시 복원할 마지막 화면 위치 (인덱스 대신 id 로 저장해 정렬/추가에도 안정적)
#[derive(Serialize, Deserialize, Default, Clone, PartialEq)]
pub struct UiState {
    /// "domain" | "settings" | "allsites" | "account"
    #[serde(default)]
    pub view: String,
    /// 마지막 선택 고객 id (0=없음)
    #[serde(default)]
    pub customer_id: u64,
    /// 마지막 선택 도메인 id (0=없음)
    #[serde(default)]
    pub domain_id: u64,
    /// 도메인 화면 탭: "info"|"migrate"|"cms"|"eond"|"history"
    #[serde(default)]
    pub tab: String,
    /// 설정 화면 탭: "connect"|"ssh"|"bulk"|"moddel"|"disk"|"backup"
    #[serde(default)]
    pub settings_tab: String,
}

/// 스캔 결과 캐시 항목
#[derive(Serialize, Deserialize, Default, Clone)]
pub struct CachedSite {
    #[serde(default)]
    pub account: String,
    #[serde(default)]
    pub domain: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub version: String,
    /// 최신/업데이트 상태
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub git: bool,
    #[serde(default)]
    pub file_bytes: u64,
    #[serde(default)]
    pub db_bytes: u64,
    /// HestiaCP 웹도메인 생성일 (YYYY-MM-DD). 없으면 빈 문자열.
    #[serde(default)]
    pub created: String,
    /// public_html 소유/권한 "소유자:chmod" (예: "rokmc:755"). 없으면 빈 문자열.
    #[serde(default)]
    pub perm: String,
}

/// 앱 설정 (HestiaCP 연동 등). 암호화 저장.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Settings {
    #[serde(default)]
    pub hestia_host: String,
    #[serde(default)]
    pub hestia_port: String,
    /// HestiaCP API 접근 해시 (관리자 > 서버설정 > API)
    #[serde(default)]
    pub hestia_hash: String,
    /// SSL 인증서 검증 (자체서명이면 끄기)
    #[serde(default)]
    pub ssl_verify: bool,
    /// 일괄 작업용 서버 SSH 호스트 (비우면 hestia_host 사용)
    #[serde(default)]
    pub ssh_host: String,
    /// SSH 유저 — root 직접 로그인 대신 sudo 권한 계정(예: tong)
    #[serde(default)]
    pub ssh_user: String,
    /// 위 유저의 비밀번호 (SSH + sudo 공용)
    #[serde(default)]
    pub ssh_pass: String,
    /// SSH 포트 (비우면 22)
    #[serde(default)]
    pub ssh_port: String,
    /// Rhymix 모듈/레이아웃 업로드 소스 (로컬 PC의 dev/rx 경로 — 하위에 modules/ layouts/ 포함)
    #[serde(default)]
    pub rx_source_local: String,
    /// WordPress 플러그인 소스 (로컬 PC의 dev/wp 경로 — 하위에 wp-content/plugins/ 포함)
    #[serde(default)]
    pub wp_source_local: String,
}

impl Settings {
    pub fn port_or_default(&self) -> &str {
        if self.hestia_port.trim().is_empty() { "8083" } else { self.hestia_port.trim() }
    }
}

impl Store {
    pub fn alloc_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }
}

/// 고객 메모 1건 (그때그때 추가요청/작업사항 기록). 작성 시각 자동 기록.
#[derive(Serialize, Deserialize, Clone)]
pub struct CustomerNote {
    /// 작성(또는 최종수정) 시각 (unix 초)
    pub at: i64,
    /// 메모 본문
    pub text: String,
}

/// 고객 (예: omg)
#[derive(Serialize, Deserialize, Clone)]
pub struct Customer {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub memo: String,
    /// 고객별 메모 로그 (최신이 앞) — 추가요청/작업사항을 그때그때 기록
    #[serde(default)]
    pub notes: Vec<CustomerNote>,
    #[serde(default)]
    pub domains: Vec<Domain>,
    /// 소프트삭제 시각(unix초). Some 이면 휴지통, 30일 후 완전삭제.
    #[serde(default)]
    pub deleted_at: Option<i64>,
}

/// 도메인 작업 기록 1건 (이전/설치/업데이트 등)
#[derive(Serialize, Deserialize, Clone)]
pub struct ActivityLog {
    /// 시각 (unix 초)
    pub at: i64,
    /// 작업 제목 (예: "WordPress 설치", "전체 이전", "eondcms 🔄 업데이트")
    pub title: String,
    /// 성공 여부
    pub ok: bool,
}

/// 도메인 = 이전(마이그레이션) 단위 (예: chailow.com)
#[derive(Serialize, Deserialize, Clone)]
pub struct Domain {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub memo: String,
    /// ① 도메인 접속정보 (등록사/관리 페이지)
    #[serde(default)]
    pub access: DomainAccess,
    /// ② 현재 사이트 (ASIS)
    #[serde(default)]
    pub asis: Site,
    /// ③ 신규 사이트 (TOBE)
    #[serde(default)]
    pub tobe: Site,
    /// ④ CMS 접속정보
    #[serde(default)]
    pub cms: CmsAccess,
    /// eondcms 설치 파라미터 (HestiaCP 멀티테넌트)
    #[serde(default)]
    pub eond: EondInstall,
    /// 일반 CMS(WordPress/Rhymix/그누보드) 설치 파라미터
    #[serde(default)]
    pub cms_install: CmsInstall,
    /// 소프트삭제 시각(unix초). Some 이면 휴지통, 30일 후 완전삭제.
    #[serde(default)]
    pub deleted_at: Option<i64>,
    /// 작업 기록 (이전/설치/업데이트 이력)
    #[serde(default)]
    pub history: Vec<ActivityLog>,
    /// "작업중" 표시 (사이드바 우클릭으로 토글) — 진행 중인 도메인 눈에 띄게
    #[serde(default)]
    pub working: bool,
}

fn default_true() -> bool {
    true
}

/// eondcms 신규 설치(HestiaCP) 파라미터. 설치 스크립트 생성에 사용.
#[derive(Serialize, Deserialize, Clone)]
pub struct EondInstall {
    /// 설치 대상: true=현재(ASIS) 서버, false=신규(TOBE) 서버. 기본 현재.
    #[serde(default = "default_true")]
    pub use_asis: bool,
    /// root 직접 로그인 불가 → sudo 경유 (ssh user 접속 후 sudo -S bash -s). 기본 켜짐.
    #[serde(default = "default_true")]
    pub sudo: bool,
    /// HestiaCP 유저 (예: jokbo)
    #[serde(default)]
    pub hestia_user: String,
    /// v-add-user 용 비밀번호
    #[serde(default)]
    pub hestia_pass: String,
    /// v-add-user 용 이메일
    #[serde(default)]
    pub hestia_email: String,
    /// HestiaCP 패키지 (기본 default)
    #[serde(default)]
    pub package: String,
    /// uvicorn 포트 (127.0.0.1, 인스턴스 고유, 수동 입력)
    #[serde(default)]
    pub port: String,
    /// DB 짧은 이름 (HestiaCP가 user_ 접두어 자동 추가)
    #[serde(default)]
    pub db_name: String,
    /// DB 짧은 유저명 (HestiaCP가 user_ 접두어 자동 추가)
    #[serde(default)]
    pub db_user: String,
    #[serde(default)]
    pub db_pass: String,
    /// Rhymix/XE 테이블 접두어 (기본 xe_)
    #[serde(default)]
    pub table_prefix: String,
    #[serde(default)]
    pub admin_user: String,
    #[serde(default)]
    pub admin_pass: String,
    /// rsync 소스: dev 머신의 eondcms pythonapp 경로
    #[serde(default)]
    pub code_local: String,
}

impl Default for EondInstall {
    fn default() -> Self {
        Self {
            use_asis: true,
            sudo: true,
            hestia_user: String::new(),
            hestia_pass: String::new(),
            hestia_email: String::new(),
            package: String::new(),
            port: String::new(),
            db_name: String::new(),
            db_user: String::new(),
            db_pass: String::new(),
            table_prefix: String::new(),
            admin_user: String::new(),
            admin_pass: String::new(),
            code_local: String::new(),
        }
    }
}

impl EondInstall {
    pub fn package_or_default(&self) -> &str {
        if self.package.trim().is_empty() { "default" } else { self.package.trim() }
    }
    pub fn table_prefix_or_default(&self) -> &str {
        if self.table_prefix.trim().is_empty() { "xe_" } else { self.table_prefix.trim() }
    }
    pub fn admin_user_or_default(&self) -> &str {
        if self.admin_user.trim().is_empty() { "admin" } else { self.admin_user.trim() }
    }
}

/// CMS 종류 (WordPress/Rhymix/그누보드)
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum CmsKind {
    #[default]
    WordPress,
    Rhymix,
    Gnuboard,
}

impl CmsKind {
    pub fn label(&self) -> &'static str {
        match self {
            CmsKind::WordPress => "WordPress",
            CmsKind::Rhymix => "Rhymix",
            CmsKind::Gnuboard => "그누보드",
        }
    }
}

/// 일반 CMS(WordPress/Rhymix/그누보드) 설치·업데이트 파라미터 (HestiaCP).
#[derive(Serialize, Deserialize, Clone)]
pub struct CmsInstall {
    #[serde(default)]
    pub kind: CmsKind,
    /// 설치 대상: true=현재(ASIS), false=신규(TOBE). 기본 현재.
    #[serde(default = "default_true")]
    pub use_asis: bool,
    #[serde(default = "default_true")]
    pub sudo: bool,
    #[serde(default)]
    pub hestia_user: String,
    #[serde(default)]
    pub hestia_pass: String,
    #[serde(default)]
    pub hestia_email: String,
    #[serde(default)]
    pub package: String,
    /// DB 전체이름/유저 (입력 그대로 사용, HestiaCP는 user_ 접두어)
    #[serde(default)]
    pub db_name: String,
    #[serde(default)]
    pub db_user: String,
    #[serde(default)]
    pub db_pass: String,
    #[serde(default)]
    pub admin_user: String,
    #[serde(default)]
    pub admin_pass: String,
    /// WordPress 관리자 이메일 (필수)
    #[serde(default)]
    pub admin_email: String,
    /// 사이트 제목 (WordPress)
    #[serde(default)]
    pub site_title: String,
    /// 언어 (기본 ko_KR)
    #[serde(default)]
    pub locale: String,
    /// 버전 (기본 latest)
    #[serde(default)]
    pub version: String,
    /// Rhymix 업로드: 설치할 모듈 이름들(쉼표 구분)
    #[serde(default)]
    pub rx_modules: String,
    /// Rhymix 업로드: 설치할 레이아웃 이름들(쉼표 구분)
    #[serde(default)]
    pub rx_layouts: String,
}

impl Default for CmsInstall {
    fn default() -> Self {
        Self {
            kind: CmsKind::default(),
            use_asis: true,
            sudo: true,
            hestia_user: String::new(),
            hestia_pass: String::new(),
            hestia_email: String::new(),
            package: String::new(),
            db_name: String::new(),
            db_user: String::new(),
            db_pass: String::new(),
            admin_user: String::new(),
            admin_pass: String::new(),
            admin_email: String::new(),
            site_title: String::new(),
            locale: String::new(),
            version: String::new(),
            rx_modules: String::new(),
            rx_layouts: String::new(),
        }
    }
}

impl CmsInstall {
    pub fn package_or_default(&self) -> &str {
        if self.package.trim().is_empty() { "default" } else { self.package.trim() }
    }
    pub fn admin_user_or_default(&self) -> &str {
        if self.admin_user.trim().is_empty() { "admin" } else { self.admin_user.trim() }
    }
    pub fn locale_or_default(&self) -> &str {
        if self.locale.trim().is_empty() { "ko_KR" } else { self.locale.trim() }
    }
    pub fn version_or_default(&self) -> &str {
        if self.version.trim().is_empty() { "latest" } else { self.version.trim() }
    }
    pub fn site_title_or_default(&self) -> &str {
        if self.site_title.trim().is_empty() { "My Site" } else { self.site_title.trim() }
    }
}

/// ① 도메인 접속정보
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct DomainAccess {
    /// 관리 페이지 URL (예: cafe24.com)
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub pw: String,
    /// 도메인명 (한글 도메인 가능 → 퓨니코드 변환)
    #[serde(default)]
    pub domain: String,
    /// 네임서버 메모 (변경은 수동)
    #[serde(default)]
    pub nameservers: String,
}

/// ②③ 사이트 접속정보 (현재/신규 공용)
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Site {
    #[serde(default)]
    pub ip: String,
    /// DNS A 레코드 호스트값 (도메인이 가리키는 IP)
    #[serde(default)]
    pub dns_a: String,
    #[serde(default)]
    pub ftp_id: String,
    #[serde(default)]
    pub ftp_pw: String,
    /// 서버 루트(관리자) 계정 — 필요 시 권한 작업용
    #[serde(default)]
    pub root_id: String,
    #[serde(default)]
    pub root_pw: String,
    #[serde(default)]
    pub db_id: String,
    #[serde(default)]
    pub db_pw: String,
    #[serde(default)]
    pub db_name: String,
    /// 웹 루트 경로 (rsync 대상)
    #[serde(default)]
    pub path: String,
    // --- 고급 (기본값 사용) ---
    #[serde(default)]
    pub ssh_port: String,
    #[serde(default)]
    pub db_host: String,
    #[serde(default)]
    pub db_port: String,
}

impl Site {
    /// SSH 로그인 아이디: use_root 이고 루트 아이디가 있으면 루트, 아니면 FTP 계정
    pub fn login_id(&self, use_root: bool) -> &str {
        if use_root && !self.root_id.trim().is_empty() {
            self.root_id.trim()
        } else {
            self.ftp_id.trim()
        }
    }
    /// SSH 로그인 비밀번호 (위 규칙과 동일)
    pub fn login_pw(&self, use_root: bool) -> &str {
        if use_root && !self.root_id.trim().is_empty() {
            self.root_pw.as_str()
        } else {
            self.ftp_pw.as_str()
        }
    }
    pub fn ssh_port_or_default(&self) -> &str {
        if self.ssh_port.trim().is_empty() { "22" } else { self.ssh_port.trim() }
    }
    pub fn db_host_or_default(&self) -> &str {
        // 공유호스팅은 보통 TCP 가 막혀 있고 소켓만 열려 있어 localhost 가 안전
        if self.db_host.trim().is_empty() { "localhost" } else { self.db_host.trim() }
    }
    pub fn db_port_or_default(&self) -> &str {
        if self.db_port.trim().is_empty() { "3306" } else { self.db_port.trim() }
    }
}

/// ④ CMS 접속정보
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct CmsAccess {
    /// 관리자 로그인 URL (선택)
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub pw: String,
}
