use serde::{Deserialize, Serialize};

/// 최상위 저장소. 암호화되어 store.enc 에 직렬화된다.
#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Store {
    #[serde(default)]
    pub next_id: u64,
    #[serde(default)]
    pub customers: Vec<Customer>,
}

impl Store {
    pub fn alloc_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }
}

/// 고객 (예: omg)
#[derive(Serialize, Deserialize, Clone)]
pub struct Customer {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub memo: String,
    #[serde(default)]
    pub domains: Vec<Domain>,
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
}

fn default_true() -> bool {
    true
}

/// eondcms 신규 설치(HestiaCP) 파라미터. 설치 스크립트 생성에 사용.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct EondInstall {
    /// 설치 대상: true=현재(ASIS) 서버, false=신규(TOBE) 서버
    #[serde(default)]
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
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct CmsInstall {
    #[serde(default)]
    pub kind: CmsKind,
    /// 설치 대상: true=현재(ASIS), false=신규(TOBE)
    #[serde(default)]
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
