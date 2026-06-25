//! 백업/복원 작업: 쉘 명령 생성 + 백그라운드 실행 + 로그 스트리밍.
//!
//! 입력은 FTP 계정으로 받지만 백업은 mysqldump/rsync(=SSH) 로 수행하므로,
//! FTP id/pw 를 SSH 로그인으로 사용한다 (대부분의 공유호스팅에서 동일).

use crate::model::Site;
use crate::store;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::time::{SystemTime, UNIX_EPOCH};

/// CMS 설정파일에서 감지한 DB 접속 정보
#[derive(Default, Clone)]
pub struct DetectedDb {
    pub name: Option<String>,
    pub user: Option<String>,
    pub pass: Option<String>,
    pub host: Option<String>,
    /// 감지한 CMS 종류 (WordPress/Rhymix/XE/그누보드5/그누보드4/알수없음)
    pub cms: Option<String>,
}

impl DetectedDb {
    pub fn is_empty(&self) -> bool {
        self.name.is_none() && self.user.is_none() && self.pass.is_none() && self.host.is_none()
    }
}

/// UI 로 보내는 메시지
pub enum LogMsg {
    Line(String),
    Done { ok: bool },
    /// 접속 테스트에서 감지한 DB 정보 (is_tobe: 신규 사이트 여부)
    Detected { is_tobe: bool, db: DetectedDb },
}

/// 한 줄에서 작은/큰따옴표로 둘러싸인 토막들을 순서대로 추출
fn quoted_parts(line: &str) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    let mut parts = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' || c == '"' {
            i += 1;
            let mut s = String::new();
            while i < chars.len() && chars[i] != c {
                s.push(chars[i]);
                i += 1;
            }
            parts.push(s);
        }
        i += 1;
    }
    parts
}

/// 키가 따옴표 토막으로 나오는 형태에서 그 다음 토막(값) 추출.
/// WP `define('DB_NAME','v')`, XE `'db_hostname'=>'v'`, Rhymix `['host']='v'` 모두 처리.
fn find_quoted_after(text: &str, key: &str, must_contain: Option<&str>) -> Option<String> {
    for line in text.lines() {
        if let Some(req) = must_contain {
            if !line.contains(req) {
                continue;
            }
        }
        let parts = quoted_parts(line);
        if let Some(i) = parts.iter().position(|p| p == key) {
            if let Some(v) = parts.get(i + 1) {
                if !v.is_empty() {
                    return Some(v.clone());
                }
            }
        }
    }
    None
}

/// `$var = 'value'` 형태(그누보드4)에서 값 추출
fn find_var(text: &str, var: &str) -> Option<String> {
    for line in text.lines() {
        if line.contains(var) && line.contains('=') {
            if let Some(v) = quoted_parts(line).into_iter().find(|s| !s.is_empty()) {
                return Some(v);
            }
        }
    }
    None
}

fn detect_cms(text: &str) -> String {
    if text.contains("$config['db']") || text.contains("['master']") {
        "Rhymix".into()
    } else if text.contains("db_hostname") || text.contains("$db_info") {
        "XE".into()
    } else if text.contains("G5_MYSQL") {
        "그누보드5".into()
    } else if text.contains("$mysql_host") {
        "그누보드4".into()
    } else if text.contains("DB_NAME") || text.contains("wp-config") {
        "WordPress".into()
    } else {
        "알수없음".into()
    }
}

/// 접속 테스트 출력에서 CMS DB 정보 파싱 (WordPress/Rhymix/XE/그누보드).
pub fn parse_detected(text: &str) -> DetectedDb {
    let name = find_quoted_after(text, "DB_NAME", None)
        .or_else(|| find_quoted_after(text, "db_database", None))
        .or_else(|| find_quoted_after(text, "G5_MYSQL_DB", None))
        .or_else(|| find_quoted_after(text, "dbname", Some("master")))
        .or_else(|| find_var(text, "$mysql_db"));
    let user = find_quoted_after(text, "DB_USER", None)
        .or_else(|| find_quoted_after(text, "db_userid", None))
        .or_else(|| find_quoted_after(text, "G5_MYSQL_USER", None))
        .or_else(|| find_quoted_after(text, "user", Some("master")))
        .or_else(|| find_var(text, "$mysql_user"));
    let pass = find_quoted_after(text, "DB_PASSWORD", None)
        .or_else(|| find_quoted_after(text, "db_password", None))
        .or_else(|| find_quoted_after(text, "G5_MYSQL_PASSWORD", None))
        .or_else(|| find_quoted_after(text, "pass", Some("master")))
        .or_else(|| find_var(text, "$mysql_password"));
    let host = find_quoted_after(text, "DB_HOST", None)
        .or_else(|| find_quoted_after(text, "db_hostname", None))
        .or_else(|| find_quoted_after(text, "G5_MYSQL_HOST", None))
        .or_else(|| find_quoted_after(text, "host", Some("master")))
        .or_else(|| find_var(text, "$mysql_host"));
    let cms = if name.is_some() || user.is_some() || host.is_some() {
        Some(detect_cms(text))
    } else {
        None
    };
    DetectedDb { name, user, pass, host, cms }
}

/// 작업 종류
#[derive(Clone, Copy, PartialEq)]
pub enum OpKind {
    DbBackup,    // 현재 사이트 DB → 로컬 .sql.gz
    DbRestore,   // 로컬 .sql.gz → 신규 사이트 DB
    FileBackup,  // 현재 사이트 웹루트 → 로컬 (rsync pull)
    FileRestore, // 로컬 → 신규 사이트 웹루트 (rsync push)
    TestAsis,    // 현재 사이트 SSH 접속 테스트
    TestTobe,    // 신규 사이트 SSH 접속 테스트
    CertAsis,    // 현재 사이트 SSL 인증서 확인
    CertTobe,    // 신규 사이트 SSL 인증서 확인
    VerifyAsis,  // 현재 사이트 점검 (에러로그/PHP/권한)
    VerifyTobe,  // 신규 사이트 점검 (이전 검증)
    FixHtaccessAsis, // 현재 .htaccess php_flag 주석처리
    FixHtaccessTobe, // 신규 .htaccess php_flag 주석처리
    SetDbAsis,   // 현재 설정파일 DB 정보 반영
    SetDbTobe,   // 신규 설정파일 DB 정보 반영(이전 후 갱신)
    DbDirect,    // 현재 DB → 신규 DB 직접 스트리밍 (로컬 디스크 미사용)
    FileDirect,  // 현재 파일 → 신규 파일 직접 스트리밍 (tar 파이프, 로컬 디스크 미사용)
}

/// POSIX 쉘용 single-quote 이스케이프
fn sq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn epoch_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// 해당 도메인의 백업 디렉터리 (~/.local/share/hostmover/backups/<고객>/<도메인>)
pub fn domain_backup_dir(customer_name: &str, domain_name: &str) -> PathBuf {
    store::backups_root()
        .join(store::sanitize(customer_name))
        .join(store::sanitize(domain_name))
}

/// 최신 DB 백업 파일 찾기
pub fn latest_db_backup(dir: &Path) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("db_") && n.ends_with(".sql.gz"))
                .unwrap_or(false)
        })
        .collect();
    found.sort();
    found.pop()
}

/// 원격 mysql -p 인자 (비번 없으면 생략)
fn mysql_pass_arg(pass: &str) -> String {
    if pass.is_empty() { String::new() } else { format!(" -p{}", sq(pass)) }
}

/// mysql/mysqldump 접속 인자.
/// DB 호스트가 `/` 로 시작하면 소켓 경로로 보고 `--socket=` 사용, 아니면 `-h host -P port`.
fn mysql_conn(host: &str, port: &str) -> String {
    let h = host.trim();
    if h.starts_with('/') {
        format!("--socket={}", sq(h))
    } else {
        format!("-h {} -P {}", sq(h), sq(port))
    }
}

/// SSH 공통 옵션
fn ssh_e(site: &Site) -> String {
    format!("ssh -p {} -o StrictHostKeyChecking=no -o ConnectTimeout=20", site.ssh_port_or_default())
}

/// 원격 명령 앞에 PATH 보강을 붙인다.
/// jailshell/제한 SSH 에서 비대화형 PATH 에 도구가 안 잡히는 경우 대비.
fn with_path(cmd: &str) -> String {
    format!(
        "export PATH=\"$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/mysql/bin:/usr/local/sbin:/opt/bin\"; {cmd}"
    )
}

/// 원격에서 실행할 명령을 로컬 쉘 토큰으로 변환한다.
/// 원격 기본 셸(sh)에서 그대로 실행하되 PATH 만 보강한다.
/// (jailshell 은 bash 가 없을 수 있으므로 bash 래퍼를 쓰지 않는다)
fn remote_cmd(actual: &str) -> String {
    sq(&with_path(actual))
}

fn validate_site_ssh(site: &Site, use_root: bool) -> Result<(), String> {
    if site.ip.trim().is_empty() { return Err("사이트 IP가 비어 있습니다".into()); }
    if site.login_id(use_root).is_empty() { return Err("SSH 로그인 아이디가 비어 있습니다".into()); }
    Ok(())
}

/// 도메인을 ASCII(퓨니코드)로
fn to_ascii_domain(d: &str) -> String {
    let t = d.trim();
    idna::domain_to_ascii(t).unwrap_or_else(|_| t.to_string())
}

/// SSL 인증서 확인 작업 생성 (로컬 openssl 로 IP:443 에 SNI=도메인 접속).
/// DNS 전환 전에도 해당 IP 에 그 도메인 인증서가 설치됐는지 확인 가능.
fn build_cert_job(site: &Site, domain_name: &str) -> Result<Job, String> {
    let sni = to_ascii_domain(domain_name);
    if sni.is_empty() {
        return Err("도메인이 비어 있습니다".into());
    }
    let host = if !site.ip.trim().is_empty() { site.ip.trim().to_string() } else { sni.clone() };
    let script = format!(
        "command -v openssl >/dev/null || {{ echo '로컬에 openssl 이 없습니다'; exit 1; }}; \
         echo \"대상 {host}:443 (SNI={sni})\"; \
         CERT=$(echo | openssl s_client -connect {hostq}:443 -servername {sniq} 2>/dev/null); \
         if [ -z \"$CERT\" ]; then echo '✗ 443 연결 실패 또는 인증서 없음'; exit 1; fi; \
         echo \"$CERT\" | openssl x509 -noout -subject -issuer -dates -ext subjectAltName 2>/dev/null; \
         if echo \"$CERT\" | openssl x509 -noout -checkend 0 >/dev/null 2>&1; then echo '=> ✓ 인증서 설치됨 (현재 유효)'; \
         else echo '=> ✗ 인증서 만료되었거나 확인 불가'; fi",
        host = host,
        sni = sni,
        hostq = sq(&host),
        sniq = sq(&sni),
    );
    Ok(Job {
        title: format!("인증서 확인 : {domain_name}"),
        script,
        sshpass: String::new(),
        env: Vec::new(),
        note: String::new(),
    })
}

/// 사이트 점검 작업: SSH 로 들어가 PHP 버전/웹루트 권한/에러로그를 모아 500 등 원인 진단.
fn build_verify_job(site: &Site, label: &str, domain_name: &str, use_root: bool) -> Result<Job, String> {
    validate_site_ssh(site, use_root)?;
    let webroot = site.path.trim().trim_end_matches('/');
    let wr_cands = if webroot.is_empty() {
        String::new()
    } else {
        format!("\"{webroot}\" \"{webroot}/httpdocs\" ")
    };
    let remote = format!(
        "echo '== 사이트 점검 =='; id 2>/dev/null; \
         echo '-- PHP --'; (php -v 2>/dev/null | head -1) || echo 'php cli 없음(CGI/FPM일 수 있음)'; \
         WR=''; for d in {wr}\"$HOME/httpdocs\" \"$HOME/html\" \"$HOME/public_html\" \"$HOME\" .; do \
           if [ -f \"$d/wp-config.php\" ] || [ -f \"$d/index.php\" ] || [ -f \"$d/.htaccess\" ]; then WR=\"$d\"; break; fi; done; \
         [ -z \"$WR\" ] && WR=\"$HOME\"; \
         echo \"-- 웹루트 $WR --\"; ls -la \"$WR\" 2>/dev/null | head -15; \
         echo '-- 설정/권한(wp-config/.htaccess) --'; ls -la \"$WR\"/wp-config.php \"$WR\"/.htaccess 2>/dev/null; \
         echo '-- PHP 에러로그 (최근 25줄) --'; found=0; \
         for l in \"$WR/error_log\" \"$WR/../logs/error_log\" \"$HOME/logs/error_log\" \"$WR/wp-content/debug.log\" \"$WR/php_errorlog\"; do \
           [ -f \"$l\" ] && found=1 && echo \"== $l ==\" && tail -n 25 \"$l\" 2>/dev/null; done; \
         [ \"$found\" = 0 ] && echo '에러로그 못 찾음 (호스팅 패널 로그 확인)'; \
         echo '(점검 끝)'",
        wr = wr_cands,
    );
    let script = format!(
        "sshpass -e {ssh} {user}@{host} {remote}",
        ssh = ssh_e(site),
        user = sq(site.login_id(use_root)),
        host = sq(site.ip.trim()),
        remote = remote_cmd(&remote),
    );
    Ok(Job {
        title: format!("사이트 점검 ({label}) : {domain_name}"),
        script,
        sshpass: site.login_pw(use_root).to_string(),
        env: Vec::new(),
        note: String::new(),
    })
}

/// .htaccess 의 php_flag/php_value 줄을 백업 후 주석처리 (PHP-FPM 서버 500 해결).
fn build_fix_htaccess_job(site: &Site, label: &str, domain_name: &str, use_root: bool) -> Result<Job, String> {
    validate_site_ssh(site, use_root)?;
    // 웹루트(path) → httpdocs → HOME 순으로 .htaccess 를 찾아 백업 후 php_flag/php_value 주석처리.
    // Plesk 는 웹루트가 httpdocs 하위라 path 설정 또는 httpdocs 탐색이 중요.
    let webroot = site.path.trim().trim_end_matches('/');
    let wr_cands = if webroot.is_empty() {
        String::new()
    } else {
        format!("\"{webroot}/.htaccess\" \"{webroot}/httpdocs/.htaccess\" ")
    };
    let remote = format!(
        "F=''; for c in {wr}\"$HOME/httpdocs/.htaccess\" \"$HOME/html/.htaccess\" \"$HOME/public_html/.htaccess\" \"$HOME/.htaccess\" \"./.htaccess\"; do \
           [ -f \"$c\" ] && F=\"$c\" && break; done; \
         if [ -z \"$F\" ]; then echo '.htaccess 를 못 찾음 — 신규 사이트 path(웹루트)를 정확히 설정하세요'; exit 0; fi; \
         echo \"대상: $F\"; cp -a \"$F\" \"$F.hostmover.bak\" && echo \"백업 생성: $F.hostmover.bak\"; \
         n=$(grep -ciE '^[[:space:]]*php_(flag|value)' \"$F\" 2>/dev/null); \
         sed -i -E 's/^([[:space:]]*)(php_flag|php_value)/\\1# \\2/I' \"$F\"; \
         echo \"php_flag/php_value 주석처리: ${{n}}건\"; \
         echo '-- 결과 --'; grep -niE '#[[:space:]]*php_(flag|value)' \"$F\" 2>/dev/null | head; \
         echo '완료 (문제 시 .hostmover.bak 로 복구)'",
        wr = wr_cands,
    );
    let script = format!(
        "sshpass -e {ssh} {user}@{host} {remote}",
        ssh = ssh_e(site),
        user = sq(site.login_id(use_root)),
        host = sq(site.ip.trim()),
        remote = remote_cmd(&remote),
    );
    Ok(Job {
        title: format!(".htaccess 수정 ({label}) : {domain_name}"),
        script,
        sshpass: site.login_pw(use_root).to_string(),
        env: Vec::new(),
        note: "php_flag/php_value 줄을 주석처리합니다 (.hostmover.bak 백업).".into(),
    })
}

/// 설정파일(wp-config.php 등)의 DB 정보를 사이트의 DB 칸 값으로 교체.
/// perl 로 값을 $ENV 로 주입해 특수문자(/, &, 따옴표 등)에 안전. 수정 전 백업.
/// 지원 키: WordPress(DB_*), XE(db_*), 그누보드5(G5_MYSQL_*), Rhymix(dbname 일부).
fn build_setdb_job(site: &Site, label: &str, domain_name: &str, use_root: bool) -> Result<Job, String> {
    validate_site_ssh(site, use_root)?;
    let webroot = site.path.trim().trim_end_matches('/');
    let wr_cands = if webroot.is_empty() {
        String::new()
    } else {
        format!("\"{webroot}\" \"{webroot}/httpdocs\" ")
    };
    // 원격에 export 할 새 DB 값 (sq 로 안전하게)
    let mut exports = String::new();
    exports += &format!("export HM_DBHOST={}; ", sq(site.db_host_or_default()));
    exports += &format!("export HM_DBNAME={}; ", sq(site.db_name.trim()));
    exports += &format!("export HM_DBUSER={}; ", sq(site.db_id.trim()));
    exports += &format!("export HM_DBPASS={}; ", sq(&site.db_pw));

    // 키별 perl 치환 (단일 따옴표 스크립트라 sh 가 $1/$ENV 를 건드리지 않음; \x27 = 작은따옴표)
    let reps_for = |keys: &[&str], env: &str| -> String {
        keys.iter()
            .map(|k| {
                format!(
                    "perl -0777 -i -pe 's/([\\x27\"]{k}[\\x27\"][^\\x27\"]*?[\\x27\"])[^\\x27\"]*([\\x27\"])/$1.$ENV{{{env}}}.$2/se' \"$CFG\" 2>/dev/null; "
                )
            })
            .collect()
    };
    let mut reps = String::new();
    if !site.db_name.trim().is_empty() {
        reps += &reps_for(&["DB_NAME", "db_database", "G5_MYSQL_DB", "dbname"], "HM_DBNAME");
    }
    if !site.db_id.trim().is_empty() {
        reps += &reps_for(&["DB_USER", "db_userid", "G5_MYSQL_USER"], "HM_DBUSER");
    }
    if !site.db_pw.is_empty() {
        reps += &reps_for(&["DB_PASSWORD", "db_password", "G5_MYSQL_PASSWORD"], "HM_DBPASS");
    }
    reps += &reps_for(&["DB_HOST", "db_hostname", "G5_MYSQL_HOST"], "HM_DBHOST");

    let remote = format!(
        "command -v perl >/dev/null || {{ echo '원격에 perl 이 없어 DB정보 반영 불가'; exit 1; }}; \
         {exports} CFG=''; \
         for c in {wr}\"$HOME/httpdocs\" \"$HOME/html\" \"$HOME/public_html\" \"$HOME\" .; do \
           for f in wp-config.php data/dbconfig.php files/config/db.config.php files/config/config.php config.php; do \
             [ -f \"$c/$f\" ] && CFG=\"$c/$f\" && break 2; done; done; \
         if [ -z \"$CFG\" ]; then echo '설정파일 못 찾음 — 신규 사이트 path(웹루트) 설정 필요'; exit 0; fi; \
         echo \"대상 설정파일: $CFG\"; cp -a \"$CFG\" \"$CFG.hostmover.bak\" && echo \"백업: $CFG.hostmover.bak\"; \
         {reps} \
         echo '-- 반영 결과(비번 제외) --'; grep -iE 'DB_NAME|DB_USER|DB_HOST|db_database|db_userid|db_hostname|G5_MYSQL_(DB|USER|HOST)|dbname' \"$CFG\" 2>/dev/null | grep -ivE 'password|_pass' | head -20; \
         echo '완료 (문제 시 .hostmover.bak 복구)'",
        exports = exports,
        wr = wr_cands,
        reps = reps,
    );
    let script = format!(
        "sshpass -e {ssh} {user}@{host} {remote}",
        ssh = ssh_e(site),
        user = sq(site.login_id(use_root)),
        host = sq(site.ip.trim()),
        remote = remote_cmd(&remote),
    );
    Ok(Job {
        title: format!("DB정보 반영 ({label}) : {domain_name}"),
        script,
        sshpass: site.login_pw(use_root).to_string(),
        env: Vec::new(),
        note: "설정파일의 DB 접속정보를 신규 값으로 교체합니다 (.hostmover.bak 백업).".into(),
    })
}

// ===== eondcms 설치 (HestiaCP 멀티테넌트) =====
use crate::model::{CmsInstall, CmsKind, EondInstall};

/// 내장 nginx 프록시 템플릿 (HTTP). 도메인 무관(%placeholder% 사용), 포트만 8001 하드코딩 → 설치 시 sed.
const EONDCMS_TPL: &str = r#"server {
    listen      %ip%:%proxy_port%;
    server_name %domain_idn% %alias_idn%;
    error_log   /var/log/%web_system%/domains/%domain%.error.log error;
    include %home%/%user%/conf/web/%domain%/nginx.forcessl.conf*;
    location ~ /\.(?!well-known\/|file) { deny all; return 404; }
    location /static/ { alias %home%/%user%/web/%domain%/pythonapp/static/; expires 30d; access_log off; }
    location /_app/   { alias %home%/%user%/web/%domain%/pythonapp/web/build/_app/; expires 30d; access_log off; }
    location /files/        { alias %home%/%user%/web/%domain%/public_html/files/;        expires 7d;  access_log off; }
    location /modules/      { alias %home%/%user%/web/%domain%/public_html/modules/;      expires 7d;  access_log off; }
    location /layouts/      { alias %home%/%user%/web/%domain%/public_html/layouts/;      expires 7d;  access_log off; }
    location /m.layouts/    { alias %home%/%user%/web/%domain%/public_html/m.layouts/;    expires 7d;  access_log off; }
    location /addons/       { alias %home%/%user%/web/%domain%/public_html/addons/;       expires 7d;  access_log off; }
    location /widgets/      { alias %home%/%user%/web/%domain%/public_html/widgets/;      expires 7d;  access_log off; }
    location /widgetstyles/ { alias %home%/%user%/web/%domain%/public_html/widgetstyles/; expires 7d;  access_log off; }
    location /common/       { alias %home%/%user%/web/%domain%/public_html/common/;       expires 30d; access_log off; }
    location / {
        proxy_pass              http://127.0.0.1:8001;
        proxy_http_version      1.1;
        proxy_set_header        Host                    $host;
        proxy_set_header        X-Real-IP               $remote_addr;
        proxy_set_header        X-Forwarded-For         $proxy_add_x_forwarded_for;
        proxy_set_header        X-Forwarded-Proto       $scheme;
        proxy_set_header        Upgrade                 $http_upgrade;
        proxy_set_header        Connection              "upgrade";
        proxy_read_timeout      90s;
        proxy_send_timeout      90s;
        proxy_buffering         off;
        client_max_body_size    50M;
        access_log /var/log/%web_system%/domains/%domain%.log combined;
    }
    location /error/ { alias %home%/%user%/web/%domain%/document_errors/; }
    include %home%/%user%/conf/web/%domain%/nginx.conf_*;
}"#;

/// 내장 nginx 프록시 템플릿 (HTTPS/SSL).
const EONDCMS_STPL: &str = r#"server {
    listen      %ip%:%proxy_ssl_port% ssl;
    http2       on;
    server_name %domain_idn% %alias_idn%;
    ssl_certificate      %ssl_pem%;
    ssl_certificate_key  %ssl_key%;
    error_log   /var/log/%web_system%/domains/%domain%.error.log error;
    location ~ /\.(?!well-known\/|file) { deny all; return 404; }
    location /static/ { alias %home%/%user%/web/%domain%/pythonapp/static/; expires 30d; access_log off; }
    location /_app/   { alias %home%/%user%/web/%domain%/pythonapp/web/build/_app/; expires 30d; access_log off; }
    location /files/        { alias %home%/%user%/web/%domain%/public_html/files/;        expires 7d;  access_log off; }
    location /modules/      { alias %home%/%user%/web/%domain%/public_html/modules/;      expires 7d;  access_log off; }
    location /layouts/      { alias %home%/%user%/web/%domain%/public_html/layouts/;      expires 7d;  access_log off; }
    location /m.layouts/    { alias %home%/%user%/web/%domain%/public_html/m.layouts/;    expires 7d;  access_log off; }
    location /addons/       { alias %home%/%user%/web/%domain%/public_html/addons/;       expires 7d;  access_log off; }
    location /widgets/      { alias %home%/%user%/web/%domain%/public_html/widgets/;      expires 7d;  access_log off; }
    location /widgetstyles/ { alias %home%/%user%/web/%domain%/public_html/widgetstyles/; expires 7d;  access_log off; }
    location /common/       { alias %home%/%user%/web/%domain%/public_html/common/;       expires 30d; access_log off; }
    location / {
        proxy_pass              http://127.0.0.1:8001;
        proxy_http_version      1.1;
        proxy_set_header        Host                    $host;
        proxy_set_header        X-Real-IP               $remote_addr;
        proxy_set_header        X-Forwarded-For         $proxy_add_x_forwarded_for;
        proxy_set_header        X-Forwarded-Proto       $scheme;
        proxy_set_header        Upgrade                 $http_upgrade;
        proxy_set_header        Connection              "upgrade";
        proxy_read_timeout      90s;
        proxy_send_timeout      90s;
        proxy_buffering         off;
        client_max_body_size    50M;
        access_log /var/log/%web_system%/domains/%domain%.log combined;
    }
    location /error/ { alias %home%/%user%/web/%domain%/document_errors/; }
    include %home%/%user%/conf/web/%domain%/nginx.ssl.conf_*;
}"#;

/// eondcms 원격 실행 래퍼.
/// sudo=true: `ssh user@host "sudo -S -p '' bash -s"` 에 (비번\n + 스크립트)를 stdin 파이프 → 전체 root 실행.
/// sudo=false: 직접 root 로그인 가정, remote_cmd 로 실행.
/// 반환: (로컬 실행 스크립트, SSHPASS, 추가 env)
fn eondcms_exec(server: &Site, raw: &str, use_root: bool, sudo: bool) -> (String, String, Vec<(String, String)>) {
    let pw = server.login_pw(use_root).to_string();
    let u = sq(server.login_id(use_root));
    let h = sq(server.ip.trim());
    if sudo {
        let script = format!(
            "{{ printf '%s\\n' \"$HM_SUDOPW\"; cat <<'HM_EOF'\n{raw}\nHM_EOF\n}} | sshpass -e {ssh} {u}@{h} \"sudo -S -p '' bash -s\"",
            ssh = ssh_e(server), u = u, h = h, raw = raw,
        );
        (script, pw.clone(), vec![("HM_SUDOPW".to_string(), pw)])
    } else {
        let script = format!(
            "sshpass -e {ssh} {u}@{h} {rc}",
            ssh = ssh_e(server), u = u, h = h, rc = remote_cmd(raw),
        );
        (script, pw, Vec::new())
    }
}

fn eondcms_validate(server: &Site, eond: &EondInstall, use_root: bool) -> Result<(), String> {
    if server.ip.trim().is_empty() { return Err("설치 대상 서버 IP가 비어 있습니다".into()); }
    if !use_root { return Err("eondcms 설치는 root 권한 필요 — '루트로 실행'을 켜고 서버루트 계정을 입력하세요".into()); }
    if server.login_id(use_root).is_empty() { return Err("서버 루트 로그인 아이디가 비어 있습니다".into()); }
    if eond.hestia_user.trim().is_empty() { return Err("HestiaCP 유저가 비어 있습니다".into()); }
    Ok(())
}

/// ① HestiaCP 리소스 생성 (유저/도메인/DB, 멱등)
pub fn build_eondcms_resources(server: &Site, eond: &EondInstall, domain_name: &str, use_root: bool) -> Result<Job, String> {
    eondcms_validate(server, eond, use_root)?;
    let domain = to_ascii_domain(domain_name);
    if eond.db_name.trim().is_empty() || eond.db_user.trim().is_empty() {
        return Err("DB 이름/유저가 비어 있습니다".into());
    }
    let head = format!(
        "set -e\nexport PATH=\"$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin\"\nVBIN=/usr/local/hestia/bin\nVUSER={u}\nDOMAIN={d}\nPKG={pkg}\nEMAIL={em}\nUPASS={up}\nDBNAME={dbn}\nDBUSER={dbu}\nDBPASS={dbp}\n",
        u = sq(eond.hestia_user.trim()), d = sq(&domain), pkg = sq(eond.package_or_default()),
        em = sq(eond.hestia_email.trim()), up = sq(&eond.hestia_pass),
        dbn = sq(eond.db_name.trim()), dbu = sq(eond.db_user.trim()), dbp = sq(&eond.db_pass),
    );
    // DBNAME/DBUSER 는 입력 그대로(전체 이름). v-add-database 만 HestiaCP 가 접두어를 붙이므로
    // 이미 'VUSER_' 로 시작하면 그 부분을 떼서 전달(이중접두어 방지). 기존 DB면 v-list 로 skip.
    let body = r#"FULLDB="$DBNAME"
DBSHORT="${DBNAME#${VUSER}_}"
DBUSHORT="${DBUSER#${VUSER}_}"
echo "== [리소스] user=$VUSER domain=$DOMAIN db=$FULLDB =="
$VBIN/v-list-user "$VUSER" >/dev/null 2>&1 || $VBIN/v-add-user "$VUSER" "$UPASS" "$EMAIL" "$PKG"
$VBIN/v-list-web-domain "$VUSER" "$DOMAIN" >/dev/null 2>&1 || $VBIN/v-add-web-domain "$VUSER" "$DOMAIN"
$VBIN/v-list-database "$VUSER" "$FULLDB" >/dev/null 2>&1 || $VBIN/v-add-database "$VUSER" "$DBSHORT" "$DBUSHORT" "$DBPASS" mysql
echo "리소스 OK (DB=$FULLDB)"; ls -ld "/home/$VUSER/web/$DOMAIN" 2>/dev/null || true"#;
    let remote = format!("{head}{body}");
    let (script, sshpass, env) = eondcms_exec(server, &remote, use_root, eond.sudo);
    Ok(Job {
        title: format!("eondcms ① 리소스 생성 : {domain_name}"),
        script,
        sshpass,
        env,
        note: "HestiaCP 유저/도메인/DB 생성(이미 있으면 건너뜀)".into(),
    })
}

/// ② 코드 업로드 (dev → 서버 $APPDIR, rsync push). root 로 올리고 ③에서 chown.
pub fn build_eondcms_upload(server: &Site, eond: &EondInstall, domain_name: &str, use_root: bool) -> Result<Job, String> {
    eondcms_validate(server, eond, use_root)?;
    if eond.code_local.trim().is_empty() { return Err("코드 소스 경로(code_local)가 비어 있습니다".into()); }
    let domain = to_ascii_domain(domain_name);
    // tong 같은 sudo 유저는 남의 홈(/home/<유저>)에 못 쓰므로 /tmp 스테이징에 올리고 ③에서 root 가 복사
    let staging = format!("/tmp/hm-eond-{}", store::sanitize(&domain));
    let src = eond.code_local.trim().trim_end_matches('/');
    // 소스의 .rsyncignore(검증된 제외: mobile/·venv/·node_modules·data/ 등) 우선 + 안전망 제외.
    // --delete --delete-excluded 로 스테이징을 필터링된 소스와 정확히 일치(이전에 잘못 올라간 것 자동 정리).
    let script = format!(
        "SRC={src}\nEF=\"\"\n[ -f \"$SRC/.rsyncignore\" ] && EF=\"--exclude-from=$SRC/.rsyncignore\" && echo '[.rsyncignore 적용]'\n\
         sshpass -e rsync -az --mkpath --delete --delete-excluded --info=stats1 $EF \
         --exclude='.git/' --exclude='.venv/' --exclude='venv/' --exclude='node_modules/' --exclude='web/node_modules/' \
         --exclude='mobile/' --exclude='__pycache__/' --exclude='*.pyc' --exclude='logs/' --exclude='backup/' --exclude='data/' \
         -e {sshopt} \"$SRC/\" {user}@{host}:{staging}/",
        src = sq(src), sshopt = sq(&ssh_e(server)),
        user = sq(server.login_id(use_root)), host = sq(server.ip.trim()), staging = sq(&staging),
    );
    Ok(Job {
        title: format!("eondcms ② 코드 업로드 : {domain_name}"),
        script,
        sshpass: server.login_pw(use_root).to_string(),
        env: Vec::new(),
        note: format!("{src}/ → {staging}/ (.rsyncignore 적용, mobile/venv/node_modules 제외). ③에서 $APPDIR 복사"),
    })
}

/// ③ 설치 마무리 (venv/.env/alembic/systemd/SSL/nginx). 멱등.
pub fn build_eondcms_finalize(server: &Site, eond: &EondInstall, domain_name: &str, use_root: bool) -> Result<Job, String> {
    eondcms_validate(server, eond, use_root)?;
    if eond.port.trim().is_empty() { return Err("포트가 비어 있습니다 (수동 입력)".into()); }
    if eond.db_name.trim().is_empty() { return Err("DB 이름이 비어 있습니다".into()); }
    let domain = to_ascii_domain(domain_name);
    let staging = format!("/tmp/hm-eond-{}", store::sanitize(&domain));
    let head = format!(
        "set -e\nexport PATH=\"$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/local/mysql/bin\"\nVBIN=/usr/local/hestia/bin\nTPLDIR=/usr/local/hestia/data/templates/web/nginx\nVUSER={u}\nDOMAIN={d}\nPORT={p}\nSTAGING={st}\nDBNAME={dbn}\nDBUSER={dbu}\nDBPASS={dbp}\nTPREFIX={tp}\nADMINU={au}\nADMINP={ap}\n",
        u = sq(eond.hestia_user.trim()), d = sq(&domain), p = sq(eond.port.trim()), st = sq(&staging),
        dbn = sq(eond.db_name.trim()), dbu = sq(eond.db_user.trim()), dbp = sq(&eond.db_pass),
        tp = sq(eond.table_prefix_or_default()), au = sq(eond.admin_user_or_default()), ap = sq(&eond.admin_pass),
    );
    let body1 = r#"APPDIR=/home/$VUSER/web/$DOMAIN/pythonapp
SERVICE=eondcms-$VUSER
FULLDB="$DBNAME"
FULLDBU="$DBUSER"
echo "== [마무리] $DOMAIN 포트 $PORT =="
if ss -ltn 2>/dev/null | grep -q ":$PORT "; then echo "✗ 포트 $PORT 사용 중 — 다른 포트로"; exit 1; fi
if [ -d "$STAGING" ]; then echo "스테이징 → $APPDIR 복사"; mkdir -p "$APPDIR"; cp -a "$STAGING/." "$APPDIR/"; fi
if [ ! -f "$APPDIR/app/main.py" ]; then echo "✗ 코드 없음: $APPDIR (먼저 ② 코드 업로드)"; exit 1; fi
if [ ! -d "$APPDIR/web/build" ]; then echo "※ web/build 없음 — 정적이 깨질 수 있음(로컬 npm run build 후 재업로드 권장)"; fi
chown -R "$VUSER:$VUSER" "$APPDIR"
sudo -u "$VUSER" mkdir -p "$APPDIR/logs"
echo "-- venv/poetry --"
sudo -u "$VUSER" bash -lc "cd '$APPDIR' && python3.11 -m venv .venv && .venv/bin/pip install -q -U pip poetry && .venv/bin/poetry config virtualenvs.create false --local && .venv/bin/poetry install --no-root --only main"
if [ ! -f "$APPDIR/.env" ]; then
  SECRET=$(openssl rand -hex 32)
  FERNET=$(sudo -u "$VUSER" "$APPDIR/.venv/bin/python" -c 'from cryptography.fernet import Fernet;print(Fernet.generate_key().decode())')
  cat > "$APPDIR/.env" <<ENVEOF
ENV=production
DATABASE_URL=mysql+aiomysql://$FULLDBU:$DBPASS@127.0.0.1:3306/$FULLDB?charset=utf8mb4
SECRET_KEY=$SECRET
FERNET_KEY=$FERNET
ALGORITHM=HS256
TABLE_PREFIX=$TPREFIX
ADMIN_USERNAME=$ADMINU
ADMIN_PASSWORD=$ADMINP
ALLOWED_ORIGINS=["https://$DOMAIN"]
ENVEOF
  chown "$VUSER:$VUSER" "$APPDIR/.env"; chmod 600 "$APPDIR/.env"; echo ".env 생성"
else echo ".env 유지(기존 키 보존)"; fi
# TABLE_PREFIX 는 비밀이 아니므로 항상 설정값과 동기화 (seed 접두어 불일치=테이블없음 방지)
sudo -u "$VUSER" bash -lc "cd '$APPDIR' && if grep -q '^TABLE_PREFIX=' .env; then sed -i 's/^TABLE_PREFIX=.*/TABLE_PREFIX=$TPREFIX/' .env; else echo 'TABLE_PREFIX=$TPREFIX' >> .env; fi"
echo "TABLE_PREFIX=$TPREFIX 동기화(.env)"
echo "-- Rhymix 베이스 시드 (접두어 무관 멱등) --"
MQ="MYSQL_PWD='$DBPASS' mysql -h 127.0.0.1 -P 3306 -u '$FULLDBU' '$FULLDB' -N"
EXIST=$(sudo -u "$VUSER" bash -lc "$MQ -e \"SHOW TABLES LIKE '%_document_categories'\"" 2>/dev/null | head -1)
if [ -n "$EXIST" ]; then
  echo "Rhymix 테이블 이미 존재($EXIST) — 적재 건너뜀(데이터 보호)"
elif [ -f "$APPDIR/rhymix_base.sql" ]; then
  echo "rhymix_base.sql 적재 → $FULLDB"
  sudo -u "$VUSER" bash -lc "MYSQL_PWD='$DBPASS' mysql -h 127.0.0.1 -P 3306 -u '$FULLDBU' '$FULLDB' < '$APPDIR/rhymix_base.sql'" && echo "Rhymix 베이스 적재 완료"
else
  echo "rhymix_base.sql 없음 — Rhymix 베이스 없이 진행 (빈 DB면 eondcms 부팅 실패 가능)"
fi
# 실제 적재된 Rhymix 접두어를 자동 감지 → .env 강제 동기화 (입력칸 오타 무시)
DET=$(sudo -u "$VUSER" bash -lc "$MQ -e \"SHOW TABLES LIKE '%_document_categories'\"" 2>/dev/null | head -1 | sed 's/document_categories$//')
if [ -n "$DET" ]; then
  echo "Rhymix 접두어 자동감지: $DET → .env TABLE_PREFIX 반영"
  sudo -u "$VUSER" bash -lc "cd '$APPDIR' && if grep -q '^TABLE_PREFIX=' .env; then sed -i 's/^TABLE_PREFIX=.*/TABLE_PREFIX=$DET/' .env; else echo 'TABLE_PREFIX=$DET' >> .env; fi"
  TPREFIX="$DET"
fi
echo "-- eond_ 스키마(완성본) 적재: eond_ 데이터 없을 때만 (DROP 데이터손실 방지) --"
if [ -f "$APPDIR/eond_schema.sql" ]; then
  HASC=$(sudo -u "$VUSER" bash -lc "$MQ -e \"SELECT COUNT(*) FROM information_schema.columns WHERE table_schema='$FULLDB' AND table_name='eond_projects' AND column_name='deleted_at'\"" 2>/dev/null | head -1)
  ROWS=$(sudo -u "$VUSER" bash -lc "$MQ -e \"SELECT COUNT(*) FROM eond_projects\"" 2>/dev/null | head -1)
  if [ "${HASC:-0}" != "0" ]; then
    echo "eond_ 스키마 이미 완성(deleted_at 존재) — 건너뜀"
  elif [ -n "$ROWS" ] && [ "$ROWS" != "0" ]; then
    echo "※ eond_ 데이터 존재(eond_projects=$ROWS) + 스키마 불완전 → 적재 스킵(데이터 보호). 수동 보강 필요"
  else
    echo "eond_ 비어있고 불완전 → eond_schema.sql 적재(DROP+CREATE, 누락 보강)"
    sudo -u "$VUSER" bash -lc "MYSQL_PWD='$DBPASS' mysql -h 127.0.0.1 -P 3306 -u '$FULLDBU' '$FULLDB' < '$APPDIR/eond_schema.sql'" && echo "eond_schema.sql 적재 완료"
  fi
else echo "eond_schema.sql 없음 — create_all 로만 진행(불완전 가능: 대시보드 500 위험)"; fi
echo "-- DB 초기화: ① 모델 테이블 create_all 먼저 --"
sudo -u "$VUSER" bash -lc "cd '$APPDIR' && .venv/bin/python -c 'import asyncio; from app.db_bootstrap import setup_eond_tables; asyncio.run(setup_eond_tables())'" \
  || echo "※ 부트스트랩 일부 경고(Rhymix rx_ 백필 등) — 빈 DB에선 정상, eond_ 테이블 생성은 진행됨"
echo "-- ② alembic stamp head (모델=최신 기준으로 표시) --"
sudo -u "$VUSER" bash -lc "cd '$APPDIR' && .venv/bin/alembic stamp head" && echo "stamp head OK"
echo "-- eond_projects 존재 확인 --"
sudo -u "$VUSER" bash -lc "cd '$APPDIR' && .venv/bin/alembic current" 2>/dev/null || true
echo "-- 관리자 비번 세팅 (.env ADMIN_PASSWORD → <prefix>member, seed의 *LOCKED* 해소) --"
sudo -u "$VUSER" bash -lc "cd '$APPDIR' && .venv/bin/python -c \"import re,sqlalchemy as sa; from app.config import settings; from app.routers.auth import _hash_password_bcrypt as H; e=sa.create_engine(re.sub('[+]aiomysql','+pymysql',settings.database_url)); c=e.connect(); r=c.execute(sa.text('UPDATE '+settings.table_prefix+'member SET password=:p WHERE user_id=:u'),{'p':H(settings.admin_password),'u':settings.admin_username}); c.commit(); print('admin 비번 세팅 행수:', r.rowcount, settings.admin_username)\"" \
  || echo "※ 관리자 비번 세팅 실패 — 수동 필요(아래 안내)"
echo "-- systemd --"
cat > "/etc/systemd/system/$SERVICE.service" <<UNITEOF
[Unit]
Description=eondcms FastAPI ($VUSER / $DOMAIN)
After=network.target mysql.service
[Service]
Type=simple
User=$VUSER
Group=$VUSER
WorkingDirectory=$APPDIR
EnvironmentFile=$APPDIR/.env
ExecStart=$APPDIR/.venv/bin/uvicorn app.main:app --host 127.0.0.1 --port $PORT --workers 2 --proxy-headers --forwarded-allow-ips=127.0.0.1
Restart=on-failure
RestartSec=3
StandardOutput=append:$APPDIR/logs/server.log
StandardError=append:$APPDIR/logs/server.err.log
[Install]
WantedBy=multi-user.target
UNITEOF
systemctl daemon-reload
systemctl enable "$SERVICE" >/dev/null 2>&1 || true
systemctl restart "$SERVICE"
echo "서비스 재시작됨(새 .env/포트/코드 반영). 기동 대기(최대 30초)..."
HC=000
for i in 1 2 3 4 5 6 7 8 9 10; do
  sleep 3
  HC=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/" 2>/dev/null || true)
  if [ "$HC" != "000" ]; then break; fi
done
echo "uvicorn 헬스체크: $HC (200/302=정상, 500=앱 부팅됨/런타임오류, 000=미기동)"
if [ "$HC" = "000" ]; then
  echo "※ uvicorn 미응답 — systemd 상태 & 최근 로그:"
  systemctl --no-pager -l status "$SERVICE" 2>/dev/null | head -12
  journalctl -u "$SERVICE" -n 40 --no-pager 2>/dev/null
  echo "-- server.err.log --"; tail -n 40 "$APPDIR/logs/server.err.log" 2>/dev/null
fi
echo "-- SSL (proxy-tpl 보다 먼저) --"
$VBIN/v-add-letsencrypt-domain "$VUSER" "$DOMAIN" || echo "※ SSL 발급 보류 — DNS A레코드가 이 서버를 가리키는지 확인"
echo "-- nginx 템플릿 (포트 치환) --"
"#;
    let body2 = r#"sed -i "s/8001/$PORT/g" "$TPLDIR/eondcms-$PORT.tpl" "$TPLDIR/eondcms-$PORT.stpl"
chown root:root "$TPLDIR/eondcms-$PORT.tpl" "$TPLDIR/eondcms-$PORT.stpl"
chmod 644 "$TPLDIR/eondcms-$PORT.tpl" "$TPLDIR/eondcms-$PORT.stpl"
$VBIN/v-change-web-domain-proxy-tpl "$VUSER" "$DOMAIN" "eondcms-$PORT"
echo "-- 최종 확인 --"
curl -sI "https://$DOMAIN" 2>/dev/null | head -1 || true
echo "== eondcms 설치 완료: https://$DOMAIN (관리자 $ADMINU) =="
"#;
    let remote = eondcms_finalize_remote(&head, body1, body2);
    let (script, sshpass, env) = eondcms_exec(server, &remote, use_root, eond.sudo);
    Ok(Job {
        title: format!("eondcms ③ 설치 마무리 : {domain_name}"),
        script,
        sshpass,
        env,
        note: "스테이징→앱 복사 + venv/.env/alembic/systemd/SSL/nginx 설정".into(),
    })
}

/// 🔄 코드 업데이트 (이미 설치된 인스턴스): 스테이징→앱 복사 + 의존성 + DB동기화 + 재시작.
/// 설치의 무거운 단계(v-*·SSL·nginx)는 생략. 흐름: ② 코드 업로드 → 🔄 업데이트.
pub fn build_eondcms_update(server: &Site, eond: &EondInstall, domain_name: &str, use_root: bool) -> Result<Job, String> {
    eondcms_validate(server, eond, use_root)?;
    if eond.port.trim().is_empty() { return Err("포트가 비어 있습니다 (설치 시 사용한 포트)".into()); }
    let domain = to_ascii_domain(domain_name);
    let staging = format!("/tmp/hm-eond-{}", store::sanitize(&domain));
    let head = format!(
        "set -e\nexport PATH=\"$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/local/mysql/bin\"\nVUSER={u}\nDOMAIN={d}\nPORT={p}\nSTAGING={st}\nDBNAME={dbn}\nDBUSER={dbu}\nDBPASS={dbp}\n",
        u = sq(eond.hestia_user.trim()), d = sq(&domain), p = sq(eond.port.trim()), st = sq(&staging),
        dbn = sq(eond.db_name.trim()), dbu = sq(eond.db_user.trim()), dbp = sq(&eond.db_pass),
    );
    let body = r#"APPDIR=/home/$VUSER/web/$DOMAIN/pythonapp
SERVICE=eondcms-$VUSER
FULLDB="$DBNAME"
FULLDBU="$DBUSER"
MQ="MYSQL_PWD='$DBPASS' mysql -h 127.0.0.1 -P 3306 -u '$FULLDBU' '$FULLDB' -N"
echo "== [eondcms 업데이트] $DOMAIN =="
if [ -d "$STAGING" ]; then echo "스테이징 → $APPDIR 복사"; cp -a "$STAGING/." "$APPDIR/"; else echo "※ 스테이징 없음 — 먼저 ② 코드 업로드"; fi
if [ ! -f "$APPDIR/app/main.py" ]; then echo "✗ 코드 없음: $APPDIR"; exit 1; fi
chown -R "$VUSER:$VUSER" "$APPDIR"
echo "-- 의존성(poetry) --"
sudo -u "$VUSER" bash -lc "cd '$APPDIR' && { [ -d .venv ] || python3.11 -m venv .venv; } && .venv/bin/pip install -q -U poetry && .venv/bin/poetry install --no-root --only main"
echo "-- eond_ 스키마(완성본) 적재: eond_ 데이터 없을 때만 (DROP 데이터손실 방지) --"
if [ -f "$APPDIR/eond_schema.sql" ] && [ -n "$DBNAME" ]; then
  HASC=$(sudo -u "$VUSER" bash -lc "$MQ -e \"SELECT COUNT(*) FROM information_schema.columns WHERE table_schema='$FULLDB' AND table_name='eond_projects' AND column_name='deleted_at'\"" 2>/dev/null | head -1)
  ROWS=$(sudo -u "$VUSER" bash -lc "$MQ -e \"SELECT COUNT(*) FROM eond_projects\"" 2>/dev/null | head -1)
  if [ "${HASC:-0}" != "0" ]; then
    echo "eond_ 스키마 이미 완성(deleted_at 존재) — 건너뜀"
  elif [ -n "$ROWS" ] && [ "$ROWS" != "0" ]; then
    echo "※ eond_ 데이터 존재(eond_projects=$ROWS) + 스키마 불완전 → 적재 스킵(데이터 보호). 수동 보강 필요"
  else
    echo "eond_ 비어있고 불완전 → eond_schema.sql 적재(DROP+CREATE, 누락 보강)"
    sudo -u "$VUSER" bash -lc "MYSQL_PWD='$DBPASS' mysql -h 127.0.0.1 -P 3306 -u '$FULLDBU' '$FULLDB' < '$APPDIR/eond_schema.sql'" && echo "eond_schema.sql 적재 완료"
  fi
else echo "eond_schema.sql 없음 또는 DB정보 비어있음 — 스키마 보강 생략"; fi
echo "-- DB 동기화(create_all + 부트스트랩 ALTER) --"
sudo -u "$VUSER" bash -lc "cd '$APPDIR' && .venv/bin/python -c 'import asyncio; from app.db_bootstrap import setup_eond_tables; asyncio.run(setup_eond_tables())'" || echo "※ 부트스트랩 경고(무시 가능)"
sudo -u "$VUSER" bash -lc "cd '$APPDIR' && .venv/bin/alembic stamp head" 2>/dev/null || true
echo "-- 서비스 재시작 --"
systemctl restart "$SERVICE"
echo "기동 대기(최대 30초)..."
HC=000
for i in 1 2 3 4 5 6 7 8 9 10; do
  sleep 3
  HC=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/" 2>/dev/null || true)
  if [ "$HC" != "000" ]; then break; fi
done
echo "uvicorn 헬스체크: $HC (200/302=정상)"
if [ "$HC" = "000" ]; then echo "※ 미응답 — 로그:"; journalctl -u "$SERVICE" -n 30 --no-pager 2>/dev/null; tail -n 30 "$APPDIR/logs/server.err.log" 2>/dev/null; fi
echo "== eondcms 업데이트 완료: $DOMAIN =="
"#;
    let remote = format!("{head}{body}");
    let (script, sshpass, env) = eondcms_exec(server, &remote, use_root, eond.sudo);
    Ok(Job {
        title: format!("eondcms 🔄 업데이트 : {domain_name}"),
        script,
        sshpass,
        env,
        note: "코드 반영 + 의존성 + DB동기화 + 재시작 (v-*/SSL/nginx 생략)".into(),
    })
}

// ===== HestiaCP API 연동 (고객/사이트 불러오기) =====
use crate::model::Settings;

/// HestiaCP API 호출 스크립트 생성. 해시는 -K(stdin) 설정으로 전달해 argv 노출 방지.
fn hestia_api_job(s: &Settings, cmd: &str, args: &[&str]) -> (String, Vec<(String, String)>) {
    let kflag = if s.ssl_verify { "" } else { "-k " };
    let url = format!("https://{}:{}/api/", s.hestia_host.trim(), s.port_or_default());
    let mut data = format!("--data-urlencode {} --data-urlencode {}", sq("returncode=no"), sq(&format!("cmd={cmd}")));
    for (i, a) in args.iter().enumerate() {
        data += &format!(" --data-urlencode {}", sq(&format!("arg{}={}", i + 1, a)));
    }
    let script = format!(
        "curl -sS {kflag}--max-time 30 -X POST {url} {data} -K - <<HM_CFG\ndata-urlencode = \"hash=$HM_HHASH\"\nHM_CFG",
        kflag = kflag, url = sq(&url), data = data,
    );
    (script, vec![("HM_HHASH".to_string(), s.hestia_hash.clone())])
}

fn run_capture(script: &str, env: &[(String, String)]) -> Result<String, String> {
    let mut cmd = std::process::Command::new("bash");
    cmd.arg("-c").arg(script);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().map_err(|e| format!("실행 실패: {e}"))?;
    if !out.status.success() {
        let code = out.status.code().map(|c| c.to_string()).unwrap_or_else(|| "?".into());
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim();
        let hint = match code.as_str() {
            "6" => " (호스트 DNS 조회 실패)",
            "7" => " (연결 거부 — 포트/방화벽 확인)",
            "28" => " (타임아웃)",
            "35" | "60" => " (SSL 오류 — SSL검증 끄거나 인증서 확인)",
            _ => "",
        };
        return Err(format!("curl exit {code}{hint}: {err}"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn parse_json(out: &str) -> Result<serde_json::Value, String> {
    let t = out.trim();
    if t.is_empty() {
        return Err("빈 응답 (호스트/포트/해시 확인)".into());
    }
    serde_json::from_str(t).map_err(|e| {
        let head: String = t.chars().take(160).collect();
        format!("JSON 파싱 실패({e}). 응답: {head}")
    })
}

/// 연결 확인 — 유저 수 반환
pub fn hestia_check(s: &Settings) -> Result<usize, String> {
    if s.hestia_host.trim().is_empty() || s.hestia_hash.trim().is_empty() {
        return Err("호스트와 API 해시를 입력하세요".into());
    }
    Ok(hestia_list_users(s)?.len())
}

/// 전체 유저(고객) 목록
pub fn hestia_list_users(s: &Settings) -> Result<Vec<String>, String> {
    let (script, env) = hestia_api_job(s, "v-list-users", &["json"]);
    let out = run_capture(&script, &env)?;
    let v = parse_json(&out)?;
    let obj = v.as_object().ok_or("유저 목록 형식 오류 (인증 실패일 수 있음)")?;
    Ok(obj.keys().cloned().collect())
}

/// 특정 유저의 웹도메인 목록 (도메인, IP)
pub fn hestia_list_web_domains(s: &Settings, user: &str) -> Result<Vec<(String, String)>, String> {
    let (script, env) = hestia_api_job(s, "v-list-web-domains", &[user, "json"]);
    let out = run_capture(&script, &env)?;
    let v = parse_json(&out)?;
    let obj = v.as_object().ok_or("도메인 목록 형식 오류")?;
    let mut res = Vec::new();
    for (dom, fields) in obj {
        let ip = fields.get("IP").and_then(|x| x.as_str()).unwrap_or("").to_string();
        res.push((dom.clone(), ip));
    }
    Ok(res)
}

// ===== 일반 CMS 설치 (WordPress / Rhymix / 그누보드) =====

fn cms_validate(server: &Site, c: &CmsInstall, use_root: bool, need_admin: bool) -> Result<(), String> {
    if server.ip.trim().is_empty() { return Err("설치 대상 서버 IP가 비어 있습니다".into()); }
    if !use_root { return Err("CMS 설치는 root 권한 필요 — '루트로 실행'을 켜고 서버루트 계정을 입력하세요".into()); }
    if server.login_id(use_root).is_empty() { return Err("서버 루트 로그인 아이디가 비어 있습니다".into()); }
    if c.hestia_user.trim().is_empty() { return Err("HestiaCP 유저가 비어 있습니다".into()); }
    if c.db_name.trim().is_empty() || c.db_user.trim().is_empty() || c.db_pass.is_empty() {
        return Err("DB 정보(전체이름/유저/비번)가 필요합니다".into());
    }
    if need_admin && (c.admin_pass.is_empty() || c.admin_email.trim().is_empty()) {
        return Err("관리자 비번/이메일이 필요합니다 (WordPress)".into());
    }
    Ok(())
}

/// CMS 설치 (종류별 분기)
pub fn build_cms_install(server: &Site, c: &CmsInstall, domain_name: &str, use_root: bool) -> Result<Job, String> {
    match c.kind {
        CmsKind::WordPress => build_wp_install(server, c, domain_name, use_root),
        CmsKind::Rhymix => build_git_cms_install(server, c, domain_name, use_root, &RHYMIX),
        CmsKind::Gnuboard => build_git_cms_install(server, c, domain_name, use_root, &GNUBOARD),
    }
}

/// CMS 업데이트 (종류별 분기)
pub fn build_cms_update(server: &Site, c: &CmsInstall, domain_name: &str, use_root: bool) -> Result<Job, String> {
    match c.kind {
        CmsKind::WordPress => build_wp_update(server, c, domain_name, use_root),
        CmsKind::Rhymix => build_git_cms_update(server, c, domain_name, use_root, &RHYMIX),
        CmsKind::Gnuboard => build_git_cms_update(server, c, domain_name, use_root, &GNUBOARD),
    }
}

/// git 기반 PHP CMS(Rhymix/그누보드) 설치 스펙
struct GitCms {
    name: &'static str,
    repo: &'static str,
    /// 쓰기 권한 필요한 디렉토리 (webroot 상대)
    writable_dir: &'static str,
    /// 권한 (그누보드 data 는 707)
    writable_mode: &'static str,
    /// 설치 마법사 경로
    installer_path: &'static str,
}

const RHYMIX: GitCms = GitCms {
    name: "Rhymix",
    repo: "https://github.com/rhymix/rhymix.git",
    writable_dir: "files",
    writable_mode: "u+rwX",
    installer_path: "/",
};
const GNUBOARD: GitCms = GitCms {
    name: "그누보드",
    repo: "https://github.com/gnuboard/gnuboard5.git",
    writable_dir: "data",
    writable_mode: "707",
    installer_path: "/install/",
};

/// Rhymix/그누보드 설치 — v-add-* + git clone + 권한 + DB + SSL (+ 브라우저 마법사 안내)
fn build_git_cms_install(server: &Site, c: &CmsInstall, domain_name: &str, use_root: bool, g: &GitCms) -> Result<Job, String> {
    cms_validate(server, c, use_root, false)?;
    let domain = to_ascii_domain(domain_name);
    let head = format!(
        "set -e\nexport PATH=\"$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin\"\nVBIN=/usr/local/hestia/bin\nVUSER={u}\nDOMAIN={d}\nPKG={pkg}\nEMAIL={em}\nUPASS={up}\nDBNAME={dbn}\nDBUSER={dbu}\nDBPASS={dbp}\nCMSNAME={cn}\nREPO={repo}\nWDIR={wd}\nWMODE={wm}\nINSTPATH={ip}\n",
        u = sq(c.hestia_user.trim()), d = sq(&domain), pkg = sq(c.package_or_default()),
        em = sq(c.hestia_email.trim()), up = sq(&c.hestia_pass),
        dbn = sq(c.db_name.trim()), dbu = sq(c.db_user.trim()), dbp = sq(&c.db_pass),
        cn = sq(g.name), repo = sq(g.repo), wd = sq(g.writable_dir), wm = sq(g.writable_mode), ip = sq(g.installer_path),
    );
    let body = r#"FULLDB="$DBNAME"
DBSHORT="${DBNAME#${VUSER}_}"
DBUSHORT="${DBUSER#${VUSER}_}"
WEBROOT=/home/$VUSER/web/$DOMAIN/public_html
echo "== [$CMSNAME 설치] $DOMAIN (db=$FULLDB) =="
$VBIN/v-list-user "$VUSER" >/dev/null 2>&1 || { if [ -n "$UPASS" ]; then $VBIN/v-add-user "$VUSER" "$UPASS" "$EMAIL" "$PKG"; else echo "✗ 유저 없음 + 비번 없음 — 유저부터 생성 필요"; exit 1; fi; }
$VBIN/v-list-web-domain "$VUSER" "$DOMAIN" >/dev/null 2>&1 || $VBIN/v-add-web-domain "$VUSER" "$DOMAIN"
$VBIN/v-list-database "$VUSER" "$FULLDB" >/dev/null 2>&1 || $VBIN/v-add-database "$VUSER" "$DBSHORT" "$DBUSHORT" "$DBPASS" mysql
echo "리소스 OK"
if [ -f "$WEBROOT/index.php" ] && [ -d "$WEBROOT/.git" ]; then
  echo "※ 이미 $CMSNAME 코드 존재(.git) — 클론 건너뜀(데이터 보호)"
else
  echo "-- $CMSNAME 코어 git clone --"
  find "$WEBROOT" -mindepth 1 -maxdepth 1 -exec rm -rf {} + 2>/dev/null || true
  sudo -u "$VUSER" git clone --depth 1 "$REPO" "$WEBROOT"
fi
mkdir -p "$WEBROOT/$WDIR"
chown -R "$VUSER:$VUSER" "$WEBROOT"
chmod -R "$WMODE" "$WEBROOT/$WDIR"
echo "-- SSL --"
$VBIN/v-add-letsencrypt-domain "$VUSER" "$DOMAIN" || echo "※ SSL 발급 보류 — DNS A레코드가 이 서버를 가리키는지 확인"
echo "== $CMSNAME 코드/DB 준비 완료 =="
echo "▶ 마지막 단계(브라우저): https://$DOMAIN$INSTPATH 접속 → 설치 마법사"
echo "   DB 호스트=127.0.0.1  DB이름=$FULLDB  유저=$DBUSER  비번=(입력한 DB비번)  포트=3306"
echo "   그리고 관리자 계정을 마법사에서 생성하세요. (헤드리스 자동화는 공식 CLI 없어 이 단계만 수동)"
"#;
    let raw = format!("{head}{body}");
    let (script, sshpass, env) = eondcms_exec(server, &raw, use_root, c.sudo);
    Ok(Job {
        title: format!("{} 설치 : {domain_name}", g.name),
        script,
        sshpass,
        env,
        note: "v-add-* + git clone + 권한 + DB + SSL (마법사는 브라우저)".into(),
    })
}

/// Rhymix/그누보드 업데이트 — git pull (코어 갱신)
fn build_git_cms_update(server: &Site, c: &CmsInstall, domain_name: &str, use_root: bool, g: &GitCms) -> Result<Job, String> {
    cms_validate(server, c, use_root, false)?;
    let domain = to_ascii_domain(domain_name);
    let head = format!(
        "set -e\nexport PATH=\"$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin\"\nVUSER={u}\nDOMAIN={d}\nCMSNAME={cn}\n",
        u = sq(c.hestia_user.trim()), d = sq(&domain), cn = sq(g.name),
    );
    let body = r#"WEBROOT=/home/$VUSER/web/$DOMAIN/public_html
echo "== [$CMSNAME 업데이트] $DOMAIN =="
if [ ! -d "$WEBROOT/.git" ]; then echo "✗ git 체크아웃 아님: $WEBROOT — git clone 설치본만 자동 업데이트 가능(수동 교체 필요)"; exit 1; fi
sudo -u "$VUSER" git -C "$WEBROOT" pull --ff-only
chown -R "$VUSER:$VUSER" "$WEBROOT"
echo "현재 커밋: $(sudo -u "$VUSER" git -C "$WEBROOT" rev-parse --short HEAD 2>/dev/null)"
echo "== $CMSNAME 업데이트 완료: $DOMAIN =="
echo "▶ DB 스키마 변경이 있으면 브라우저 관리자에서 마무리될 수 있습니다."
"#;
    let raw = format!("{head}{body}");
    let (script, sshpass, env) = eondcms_exec(server, &raw, use_root, c.sudo);
    Ok(Job {
        title: format!("{} 업데이트 : {domain_name}", g.name),
        script,
        sshpass,
        env,
        note: "git pull 코어 갱신".into(),
    })
}

/// WordPress 설치 — v-add-* + wp-cli(core download/config/install) + SSL
fn build_wp_install(server: &Site, c: &CmsInstall, domain_name: &str, use_root: bool) -> Result<Job, String> {
    cms_validate(server, c, use_root, true)?;
    let domain = to_ascii_domain(domain_name);
    let head = format!(
        "set -e\nexport PATH=\"$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin\"\nVBIN=/usr/local/hestia/bin\nVUSER={u}\nDOMAIN={d}\nPKG={pkg}\nEMAIL={em}\nUPASS={up}\nDBNAME={dbn}\nDBUSER={dbu}\nDBPASS={dbp}\nADMINU={au}\nADMINP={ap}\nADMINE={ae}\nTITLE={ti}\nLOCALE={lo}\nWPVER={ver}\n",
        u = sq(c.hestia_user.trim()), d = sq(&domain), pkg = sq(c.package_or_default()),
        em = sq(c.hestia_email.trim()), up = sq(&c.hestia_pass),
        dbn = sq(c.db_name.trim()), dbu = sq(c.db_user.trim()), dbp = sq(&c.db_pass),
        au = sq(c.admin_user_or_default()), ap = sq(&c.admin_pass), ae = sq(c.admin_email.trim()),
        ti = sq(c.site_title_or_default()), lo = sq(c.locale_or_default()), ver = sq(c.version_or_default()),
    );
    let body = r#"FULLDB="$DBNAME"
DBSHORT="${DBNAME#${VUSER}_}"
DBUSHORT="${DBUSER#${VUSER}_}"
WEBROOT=/home/$VUSER/web/$DOMAIN/public_html
echo "== [WordPress 설치] $DOMAIN (db=$FULLDB) =="
$VBIN/v-list-user "$VUSER" >/dev/null 2>&1 || { if [ -n "$UPASS" ]; then $VBIN/v-add-user "$VUSER" "$UPASS" "$EMAIL" "$PKG"; else echo "✗ 유저 없음 + 비번 없음 — 유저부터 생성 필요"; exit 1; fi; }
$VBIN/v-list-web-domain "$VUSER" "$DOMAIN" >/dev/null 2>&1 || $VBIN/v-add-web-domain "$VUSER" "$DOMAIN"
$VBIN/v-list-database "$VUSER" "$FULLDB" >/dev/null 2>&1 || $VBIN/v-add-database "$VUSER" "$DBSHORT" "$DBUSHORT" "$DBPASS" mysql
echo "리소스 OK"
WPCLI=/usr/local/bin/wp
if ! [ -x "$WPCLI" ]; then echo "wp-cli 설치"; curl -fsSL https://raw.githubusercontent.com/wp-cli/builds/gh-pages/phar/wp-cli.phar -o "$WPCLI" && chmod +x "$WPCLI"; fi
mkdir -p "$WEBROOT"; chown -R "$VUSER:$VUSER" "$WEBROOT"
WP="sudo -u $VUSER $WPCLI --path=$WEBROOT"
if [ "$WPVER" = latest ]; then VEROPT=""; else VEROPT="--version=$WPVER"; fi
echo "-- 코어 다운로드/설정/설치 ($LOCALE, $WPVER) --"
if $WP core is-installed 2>/dev/null; then
  echo "※ 이미 WordPress 설치됨 — 코어/설치 건너뜀(데이터 보호)"
else
  [ -f "$WEBROOT/wp-load.php" ] || $WP core download --locale="$LOCALE" $VEROPT
  [ -f "$WEBROOT/wp-config.php" ] || $WP config create --dbname="$FULLDB" --dbuser="$DBUSER" --dbpass="$DBPASS" --dbhost=127.0.0.1 --locale="$LOCALE" --skip-check
  $WP core install --url="https://$DOMAIN" --title="$TITLE" --admin_user="$ADMINU" --admin_password="$ADMINP" --admin_email="$ADMINE" --skip-email
fi
$WP language core install "$LOCALE" --activate 2>/dev/null || true
chown -R "$VUSER:$VUSER" "$WEBROOT"
echo "-- SSL --"
$VBIN/v-add-letsencrypt-domain "$VUSER" "$DOMAIN" || echo "※ SSL 발급 보류 — DNS A레코드가 이 서버를 가리키는지 확인"
echo "-- 확인 --"
curl -sI "https://$DOMAIN" 2>/dev/null | head -1 || true
echo "== WordPress 설치 완료: https://$DOMAIN (관리자 $ADMINU → /wp-admin) =="
"#;
    let raw = format!("{head}{body}");
    let (script, sshpass, env) = eondcms_exec(server, &raw, use_root, c.sudo);
    Ok(Job {
        title: format!("WordPress 설치 : {domain_name}"),
        script,
        sshpass,
        env,
        note: "v-add-* + wp-cli core download/config/install + SSL".into(),
    })
}

/// WordPress 업데이트 — wp-cli core/plugin/theme/language update
fn build_wp_update(server: &Site, c: &CmsInstall, domain_name: &str, use_root: bool) -> Result<Job, String> {
    cms_validate(server, c, use_root, false)?;
    let domain = to_ascii_domain(domain_name);
    let head = format!(
        "set -e\nexport PATH=\"$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin\"\nVUSER={u}\nDOMAIN={d}\n",
        u = sq(c.hestia_user.trim()), d = sq(&domain),
    );
    let body = r#"WEBROOT=/home/$VUSER/web/$DOMAIN/public_html
WPCLI=/usr/local/bin/wp
if ! [ -x "$WPCLI" ]; then echo "wp-cli 설치"; curl -fsSL https://raw.githubusercontent.com/wp-cli/builds/gh-pages/phar/wp-cli.phar -o "$WPCLI" && chmod +x "$WPCLI"; fi
if [ ! -f "$WEBROOT/wp-load.php" ]; then echo "✗ WordPress 미설치: $WEBROOT (먼저 설치)"; exit 1; fi
WP="sudo -u $VUSER $WPCLI --path=$WEBROOT"
echo "== [WordPress 업데이트] $DOMAIN =="
$WP core update
$WP core update-db || true
$WP plugin update --all || true
$WP theme update --all || true
$WP language core update || true
echo -n "현재 버전: "; $WP core version || true
echo "== WordPress 업데이트 완료: $DOMAIN =="
"#;
    let raw = format!("{head}{body}");
    let (script, sshpass, env) = eondcms_exec(server, &raw, use_root, c.sudo);
    Ok(Job {
        title: format!("WordPress 업데이트 : {domain_name}"),
        script,
        sshpass,
        env,
        note: "wp-cli core/plugin/theme/language update".into(),
    })
}

/// finalize 원시 remote 스크립트 조립 (head + body1 + 내장 nginx 템플릿 heredoc + body2).
/// 따옴표/heredoc 검증(`bash -n`)을 위해 분리.
fn eondcms_finalize_remote(head: &str, body1: &str, body2: &str) -> String {
    let mut remote = String::from(head);
    remote.push_str(body1);
    remote.push_str("cat > \"$TPLDIR/eondcms-$PORT.tpl\" <<'TPLEOF'\n");
    remote.push_str(EONDCMS_TPL);
    remote.push_str("\nTPLEOF\n");
    remote.push_str("cat > \"$TPLDIR/eondcms-$PORT.stpl\" <<'STPLEOF'\n");
    remote.push_str(EONDCMS_STPL);
    remote.push_str("\nSTPLEOF\n");
    remote.push_str(body2);
    remote
}

/// SSH 접속 테스트 작업 생성: 로그인 성공 여부 + 원격 도구 가용성 확인
fn build_test_job(site: &Site, label: &str, domain_name: &str, use_root: bool) -> Result<Job, String> {
    validate_site_ssh(site, use_root)?;
    // 실제 작업과 동일하게 PATH 보강 후 도구 가용성을 확인하고,
    // 절대경로 탐색으로 도구가 어디 있는지(또는 진짜 없는지) 알려준다.
    let diag = "echo '== HOSTMOVER 접속 성공 =='; id 2>/dev/null; echo \"shell=$0\"; echo \"PATH=$PATH\"; \
                echo '-- 도구(PATH 보강 후) --'; \
                for t in mysqldump mysql rsync tar gzip; do printf '%s: ' \"$t\"; command -v \"$t\" || echo '없음'; done; \
                echo '-- 절대경로 탐색 --'; \
                for p in /bin/tar /usr/bin/tar /usr/local/bin/tar /bin/gzip /usr/bin/gzip \
                         /usr/bin/mysqldump /usr/local/bin/mysqldump /usr/local/mysql/bin/mysqldump \
                         /usr/bin/rsync /usr/local/bin/rsync /bin/bash /usr/bin/bash; do \
                  [ -x \"$p\" ] && echo \"  $p\"; done; \
                echo '-- MySQL 소켓 find --'; \
                find /tmp /var/lib/mysql /var/run /usr/local/mysql /var/mysql /data -maxdepth 3 -name '*.sock' 2>/dev/null; \
                echo '-- PHP 기본 소켓(=워드프레스가 쓰는 값) --'; \
                php -r 'echo \"mysqli.default_socket=\".ini_get(\"mysqli.default_socket\").\"\\n\".\"pdo_mysql.default_socket=\".ini_get(\"pdo_mysql.default_socket\").\"\\n\";' 2>/dev/null; \
                echo '-- my.cnf socket/port --'; \
                grep -hiE '^[[:space:]]*(socket|port)[[:space:]]*=' /usr/local/mysql/my.cnf /etc/my.cnf ~/.my.cnf 2>/dev/null; \
                echo '-- CMS/DB 설정 탐색 --'; \
                R=\"$HOME\"; [ -d \"$R\" ] || R=.; hit=0; \
                for base in \"$R\" \"$R/httpdocs\" \"$R/html\" \"$R/public_html\" \"$R/www\" .; do \
                  for f in wp-config.php files/config/config.php files/config/db.config.php data/dbconfig.php config.php; do \
                    if [ -f \"$base/$f\" ]; then hit=1; echo \"CONFIG: $base/$f\"; \
                      grep -iE \"DB_NAME|DB_USER|DB_PASSWORD|DB_HOST|db_database|db_userid|db_password|db_hostname|G5_MYSQL|mysql_host|mysql_user|mysql_password|mysql_db|master\" \"$base/$f\" 2>/dev/null; \
                    fi; done; done; \
                [ \"$hit\" = 0 ] && echo 'CMS 설정파일 못 찾음 (wp/rhymix/xe/그누보드)'; \
                echo '-- 포트(8000~8099) 사용현황 / 추천 빈 포트 --'; \
                ss -ltn 2>/dev/null | grep -oE ':80[0-9][0-9]' | sort -u | tr '\\n' ' '; echo; \
                for p in $(seq 8002 8099); do ss -ltn 2>/dev/null | grep -q \":$p \" || { echo \"추천 빈 포트: $p\"; break; }; done; \
                echo '(탐색 끝)'";
    let script = format!(
        "sshpass -e {ssh} {user}@{host} {remote}",
        ssh = ssh_e(site),
        user = sq(site.login_id(use_root)),
        host = sq(site.ip.trim()),
        remote = remote_cmd(diag),
    );
    Ok(Job {
        title: format!("접속 테스트 ({label}) : {domain_name}"),
        script,
        sshpass: site.login_pw(use_root).to_string(),
        env: Vec::new(),
        note: String::new(),
    })
}

/// 빌드된 작업
pub struct Job {
    pub title: String,
    pub script: String,
    /// SSHPASS 환경변수로 넘길 비밀번호 (argv 노출 방지)
    pub sshpass: String,
    /// 추가 환경변수 (직접 이전 시 HM_ASIS/HM_TOBE 등, argv 노출 방지)
    pub env: Vec<(String, String)>,
    /// 사용자에게 보여줄 결과 경로 안내(있으면)
    pub note: String,
}

/// 작업 종류/사이트로부터 실행할 쉘 스크립트 생성.
pub fn build(
    kind: OpKind,
    customer_name: &str,
    domain_name: &str,
    asis: &Site,
    tobe: &Site,
    restore_file: Option<&Path>,
    use_root: bool,
) -> Result<Job, String> {
    // 접속 테스트는 백업 폴더가 필요 없으므로 먼저 처리
    match kind {
        OpKind::TestAsis => return build_test_job(asis, "현재 사이트", domain_name, use_root),
        OpKind::TestTobe => return build_test_job(tobe, "신규 사이트", domain_name, use_root),
        OpKind::CertAsis => return build_cert_job(asis, domain_name),
        OpKind::CertTobe => return build_cert_job(tobe, domain_name),
        OpKind::VerifyAsis => return build_verify_job(asis, "현재 사이트", domain_name, use_root),
        OpKind::VerifyTobe => return build_verify_job(tobe, "신규 사이트", domain_name, use_root),
        OpKind::FixHtaccessAsis => return build_fix_htaccess_job(asis, "현재 사이트", domain_name, use_root),
        OpKind::FixHtaccessTobe => return build_fix_htaccess_job(tobe, "신규 사이트", domain_name, use_root),
        OpKind::SetDbAsis => return build_setdb_job(asis, "현재 사이트", domain_name, use_root),
        OpKind::SetDbTobe => return build_setdb_job(tobe, "신규 사이트", domain_name, use_root),
        _ => {}
    }

    let dir = domain_backup_dir(customer_name, domain_name);
    std::fs::create_dir_all(&dir).map_err(|e| format!("백업 폴더 생성 실패: {e}"))?;

    match kind {
        OpKind::TestAsis | OpKind::TestTobe | OpKind::CertAsis | OpKind::CertTobe
        | OpKind::VerifyAsis | OpKind::VerifyTobe
        | OpKind::FixHtaccessAsis | OpKind::FixHtaccessTobe
        | OpKind::SetDbAsis | OpKind::SetDbTobe => unreachable!(),
        OpKind::DbBackup => {
            validate_site_ssh(asis, use_root)?;
            if asis.db_name.trim().is_empty() { return Err("현재 사이트 DB 이름이 비어 있습니다".into()); }
            let out = dir.join(format!("db_{}.sql.gz", epoch_secs()));
            let remote = format!(
                "mysqldump {conn} -u {user}{pass} --single-transaction --set-gtid-purged=OFF --routines --triggers --default-character-set=utf8mb4 {db}",
                conn = mysql_conn(asis.db_host_or_default(), asis.db_port_or_default()),
                user = sq(asis.db_id.trim()),
                pass = mysql_pass_arg(asis.db_pw.trim()),
                db = sq(asis.db_name.trim()),
            );
            let script = format!(
                "set -o pipefail; sshpass -e {ssh} {user}@{host} {remote} | gzip > {out}",
                ssh = ssh_e(asis),
                user = sq(asis.login_id(use_root)),
                host = sq(asis.ip.trim()),
                remote = remote_cmd(&remote),
                out = sq(&out.to_string_lossy()),
            );
            Ok(Job {
                title: format!("DB 백업 (현재→로컬) : {domain_name}"),
                script,
                sshpass: asis.login_pw(use_root).to_string(),
                env: Vec::new(),
                note: format!("저장 위치: {}", out.display()),
            })
        }
        OpKind::DbRestore => {
            validate_site_ssh(tobe, use_root)?;
            if tobe.db_name.trim().is_empty() { return Err("신규 사이트 DB 이름이 비어 있습니다".into()); }
            let file = restore_file
                .map(|p| p.to_path_buf())
                .or_else(|| latest_db_backup(&dir))
                .ok_or("복원할 DB 백업 파일이 없습니다 (먼저 DB 백업을 실행하세요)")?;
            let remote = format!(
                "mysql {conn} -u {user}{pass} {db}",
                conn = mysql_conn(tobe.db_host_or_default(), tobe.db_port_or_default()),
                user = sq(tobe.db_id.trim()),
                pass = mysql_pass_arg(tobe.db_pw.trim()),
                db = sq(tobe.db_name.trim()),
            );
            let script = format!(
                "set -o pipefail; gunzip -c {file} | sshpass -e {ssh} {user}@{host} {remote}",
                file = sq(&file.to_string_lossy()),
                ssh = ssh_e(tobe),
                user = sq(tobe.login_id(use_root)),
                host = sq(tobe.ip.trim()),
                remote = remote_cmd(&remote),
            );
            Ok(Job {
                title: format!("DB 복원 (로컬→신규) : {domain_name}"),
                script,
                sshpass: tobe.login_pw(use_root).to_string(),
                env: Vec::new(),
                note: format!("사용 파일: {}", file.display()),
            })
        }
        OpKind::FileBackup => {
            validate_site_ssh(asis, use_root)?;
            if asis.path.trim().is_empty() { return Err("현재 사이트 경로(path)가 비어 있습니다".into()); }
            let local = dir.join("files");
            std::fs::create_dir_all(&local).map_err(|e| format!("로컬 폴더 생성 실패: {e}"))?;
            let rpath = asis.path.trim_end_matches('/');
            let local_q = sq(&local.to_string_lossy());
            // rsync 우선, 실패 시 tar-over-ssh 폴백 (원격에 rsync 없을 때 대비)
            // 백업은 --delete 미사용 (로컬 자료 보호)
            let remote_tar = format!("tar -C {} -czf - .", sq(rpath));
            let script = format!(
                "set -o pipefail\n\
                 if sshpass -e rsync -az --info=progress2 -e {sshopt} {user}@{host}:{rpathq}/ {local}/; then :; else\n\
                 echo '[원격 rsync 사용 불가 → tar 폴백]'\n\
                 sshpass -e {ssh} {user}@{host} {remote} | tar -C {local} -xzf -\n\
                 fi",
                sshopt = sq(&ssh_e(asis)),
                ssh = ssh_e(asis),
                user = sq(asis.login_id(use_root)),
                host = sq(asis.ip.trim()),
                rpathq = sq(rpath),
                remote = remote_cmd(&remote_tar),
                local = local_q,
            );
            Ok(Job {
                title: format!("파일 백업 (현재→로컬) : {domain_name}"),
                script,
                sshpass: asis.login_pw(use_root).to_string(),
                env: Vec::new(),
                note: format!("저장 위치: {}/", local.display()),
            })
        }
        OpKind::FileRestore => {
            validate_site_ssh(tobe, use_root)?;
            if tobe.path.trim().is_empty() { return Err("신규 사이트 경로(path)가 비어 있습니다".into()); }
            let local = dir.join("files");
            if !local.exists() { return Err("복원할 로컬 파일이 없습니다 (먼저 파일 백업을 실행하세요)".into()); }
            let rpath = tobe.path.trim_end_matches('/');
            let local_q = sq(&local.to_string_lossy());
            // rsync 우선, 실패 시 tar-over-ssh 폴백 (원격에 rsync 없을 때 대비)
            // 복원(push)은 절대 --delete 미사용 (원격 자료 보호)
            let remote_tar = format!("tar -C {} -xzf -", sq(rpath));
            let script = format!(
                "set -o pipefail\n\
                 if sshpass -e rsync -az --info=progress2 -e {sshopt} {local}/ {user}@{host}:{rpathq}/; then :; else\n\
                 echo '[원격 rsync 사용 불가 → tar 폴백]'\n\
                 tar -C {local} -czf - . | sshpass -e {ssh} {user}@{host} {remote}\n\
                 fi",
                sshopt = sq(&ssh_e(tobe)),
                ssh = ssh_e(tobe),
                local = local_q,
                user = sq(tobe.login_id(use_root)),
                host = sq(tobe.ip.trim()),
                rpathq = sq(rpath),
                remote = remote_cmd(&remote_tar),
            );
            Ok(Job {
                title: format!("파일 복원 (로컬→신규) : {domain_name}"),
                script,
                sshpass: tobe.login_pw(use_root).to_string(),
                env: Vec::new(),
                note: format!("원본: {}/", local.display()),
            })
        }
        OpKind::DbDirect => {
            validate_site_ssh(asis, use_root)?;
            validate_site_ssh(tobe, use_root)?;
            if asis.db_name.trim().is_empty() { return Err("현재 사이트 DB 이름이 비어 있습니다".into()); }
            if tobe.db_name.trim().is_empty() { return Err("신규 사이트 DB 이름이 비어 있습니다".into()); }
            let dump = format!(
                "mysqldump {conn} -u {user}{pass} --single-transaction --set-gtid-purged=OFF --routines --triggers --default-character-set=utf8mb4 {db}",
                conn = mysql_conn(asis.db_host_or_default(), asis.db_port_or_default()),
                user = sq(asis.db_id.trim()),
                pass = mysql_pass_arg(asis.db_pw.trim()),
                db = sq(asis.db_name.trim()),
            );
            let load = format!(
                "mysql {conn} -u {user}{pass} {db}",
                conn = mysql_conn(tobe.db_host_or_default(), tobe.db_port_or_default()),
                user = sq(tobe.db_id.trim()),
                pass = mysql_pass_arg(tobe.db_pw.trim()),
                db = sq(tobe.db_name.trim()),
            );
            // 현재/신규 비번이 달라도 되도록 SSHPASS 를 단계별 env(HM_ASIS/HM_TOBE)로 분리
            let script = format!(
                "set -o pipefail\n\
                 SSHPASS=\"$HM_ASIS\" sshpass -e {ssha} {ua}@{ha} {dumpq} | \
                 SSHPASS=\"$HM_TOBE\" sshpass -e {sshb} {ub}@{hb} {loadq}",
                ssha = ssh_e(asis), ua = sq(asis.login_id(use_root)), ha = sq(asis.ip.trim()), dumpq = remote_cmd(&dump),
                sshb = ssh_e(tobe), ub = sq(tobe.login_id(use_root)), hb = sq(tobe.ip.trim()), loadq = remote_cmd(&load),
            );
            Ok(Job {
                title: format!("DB 직접 이전 (현재→신규, 무저장) : {domain_name}"),
                script,
                sshpass: String::new(),
                env: vec![("HM_ASIS".into(), asis.login_pw(use_root).to_string()), ("HM_TOBE".into(), tobe.login_pw(use_root).to_string())],
                note: "로컬 디스크에 저장하지 않고 직접 스트리밍합니다.".into(),
            })
        }
        OpKind::FileDirect => {
            validate_site_ssh(asis, use_root)?;
            validate_site_ssh(tobe, use_root)?;
            if asis.path.trim().is_empty() { return Err("현재 사이트 경로(path)가 비어 있습니다".into()); }
            if tobe.path.trim().is_empty() { return Err("신규 사이트 경로(path)가 비어 있습니다".into()); }
            let send = format!("tar -C {} -czf - .", sq(asis.path.trim_end_matches('/')));
            let recv = format!("tar -C {} -xzf -", sq(tobe.path.trim_end_matches('/')));
            let script = format!(
                "set -o pipefail\n\
                 SSHPASS=\"$HM_ASIS\" sshpass -e {ssha} {ua}@{ha} {sendq} | \
                 SSHPASS=\"$HM_TOBE\" sshpass -e {sshb} {ub}@{hb} {recvq}",
                ssha = ssh_e(asis), ua = sq(asis.login_id(use_root)), ha = sq(asis.ip.trim()), sendq = remote_cmd(&send),
                sshb = ssh_e(tobe), ub = sq(tobe.login_id(use_root)), hb = sq(tobe.ip.trim()), recvq = remote_cmd(&recv),
            );
            Ok(Job {
                title: format!("파일 직접 이전 (현재→신규, tar 파이프) : {domain_name}"),
                script,
                sshpass: String::new(),
                env: vec![("HM_ASIS".into(), asis.login_pw(use_root).to_string()), ("HM_TOBE".into(), tobe.login_pw(use_root).to_string())],
                note: "로컬 디스크에 저장하지 않고 직접 스트리밍합니다.".into(),
            })
        }
    }
}

/// 단일 Job 을 현재 스레드에서 동기 실행하며 로그를 채널로 전달한다.
/// 성공 여부를 반환하며 `Done` 메시지는 보내지 않는다(호출부 책임).
fn run_job_blocking(job: Job, tx: &Sender<LogMsg>) -> bool {
    let _ = tx.send(LogMsg::Line(format!("$ {}", job.title)));
    if !job.note.is_empty() {
        let _ = tx.send(LogMsg::Line(job.note.clone()));
    }

    // bash 로 실행 (dash 는 `set -o pipefail` 미지원)
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(&job.script)
        .env("SSHPASS", &job.sshpass)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in &job.env {
        cmd.env(k, v);
    }
    let mut child = match cmd.spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(LogMsg::Line(format!("실행 실패: {e}")));
            return false;
        }
    };

    // stdout / stderr 를 각각 별도 스레드에서 읽어 채널로 전달
    let mut handles = Vec::new();
    if let Some(out) = child.stdout.take() {
        let tx = tx.clone();
        handles.push(std::thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                let _ = tx.send(LogMsg::Line(line));
            }
        }));
    }
    if let Some(err) = child.stderr.take() {
        let tx = tx.clone();
        handles.push(std::thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                let _ = tx.send(LogMsg::Line(line));
            }
        }));
    }

    let status = child.wait();
    for h in handles {
        let _ = h.join();
    }
    let ok = matches!(status, Ok(s) if s.success());
    let _ = tx.send(LogMsg::Line(if ok { "완료 ✓".into() } else { "실패 ✗".into() }));
    ok
}

/// 단일 작업을 백그라운드 스레드에서 실행하고 로그를 채널로 흘려보낸다.
/// `repaint` 는 새 줄이 들어올 때마다 UI 를 깨우기 위한 콜백.
pub fn spawn<F: Fn() + Send + 'static>(job: Job, tx: Sender<LogMsg>, repaint: F) {
    std::thread::spawn(move || {
        let ok = run_job_blocking(job, &tx);
        let _ = tx.send(LogMsg::Done { ok });
        repaint();
    });
}

/// 접속 테스트 전용: 출력을 모아 wp-config DB 정보를 파싱해 Detected 로 보낸다.
pub fn spawn_test<F: Fn() + Send + 'static>(job: Job, is_tobe: bool, tx: Sender<LogMsg>, repaint: F) {
    std::thread::spawn(move || {
        let _ = tx.send(LogMsg::Line(format!("$ {}", job.title)));
        let mut cmd = Command::new("bash");
        cmd.arg("-c").arg(&job.script).env("SSHPASS", &job.sshpass);
        for (k, v) in &job.env {
            cmd.env(k, v);
        }
        match cmd.output() {
            Ok(o) => {
                let mut combined = String::from_utf8_lossy(&o.stdout).into_owned();
                combined.push_str(&String::from_utf8_lossy(&o.stderr));
                for line in combined.lines() {
                    let _ = tx.send(LogMsg::Line(line.to_string()));
                }
                let det = parse_detected(&combined);
                if !det.is_empty() {
                    let _ = tx.send(LogMsg::Detected { is_tobe, db: det });
                }
                let ok = o.status.success();
                let _ = tx.send(LogMsg::Line(if ok { "완료 ✓".into() } else { "실패 ✗".into() }));
                let _ = tx.send(LogMsg::Done { ok });
            }
            Err(e) => {
                let _ = tx.send(LogMsg::Line(format!("실행 실패: {e}")));
                let _ = tx.send(LogMsg::Done { ok: false });
            }
        }
        repaint();
    });
}

/// 원클릭 마이그레이션: 여러 단계를 순차 실행한다.
/// 각 단계는 실행 직전에 build() 하여, 복원 단계가 직전 백업 파일을 사용하도록 한다.
/// 한 단계라도 실패하면 즉시 중단한다.
pub fn spawn_migration<F: Fn() + Send + 'static>(
    steps: Vec<OpKind>,
    customer_name: String,
    domain_name: String,
    asis: Site,
    tobe: Site,
    use_root: bool,
    tx: Sender<LogMsg>,
    repaint: F,
) {
    std::thread::spawn(move || {
        let _ = tx.send(LogMsg::Line(format!("=== 원클릭 마이그레이션 시작: {domain_name} ===")));
        let total = steps.len();
        let mut ok_all = true;
        for (i, step) in steps.into_iter().enumerate() {
            let _ = tx.send(LogMsg::Line(format!("── [{}/{}] 단계 시작 ──", i + 1, total)));
            match build(step, &customer_name, &domain_name, &asis, &tobe, None, use_root) {
                Ok(job) => {
                    if !run_job_blocking(job, &tx) {
                        let _ = tx.send(LogMsg::Line("이 단계가 실패하여 마이그레이션을 중단합니다.".into()));
                        ok_all = false;
                        break;
                    }
                }
                Err(e) => {
                    let _ = tx.send(LogMsg::Line(format!("단계 준비 실패: {e} → 중단")));
                    ok_all = false;
                    break;
                }
            }
        }
        let _ = tx.send(LogMsg::Line(
            if ok_all { "=== 마이그레이션 완료 ✓ ===".into() } else { "=== 마이그레이션 실패 ✗ ===".into() },
        ));
        let _ = tx.send(LogMsg::Done { ok: ok_all });
        repaint();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Site;

    fn sample_site() -> Site {
        Site {
            ip: "1.2.3.4".into(),
            ftp_id: "ftpuser".into(),
            ftp_pw: "ftppass".into(),
            db_id: "dbuser".into(),
            db_pw: "db'pa ss".into(), // 작은따옴표/공백 이스케이프 검증
            db_name: "mydb".into(),
            path: "/home/www".into(),
            ..Default::default()
        }
    }

    #[test]
    fn db_backup_script_shape() {
        std::env::set_var("HOME", std::env::temp_dir());
        let j = build(OpKind::DbBackup, "omg", "ex.com", &sample_site(), &Site::default(), None, false).unwrap();
        assert!(j.script.contains("mysqldump"));
        assert!(j.script.contains("gzip >"));
        assert!(j.script.contains("sshpass -e"));
        assert!(j.script.contains("'ftpuser'@'1.2.3.4'"));
        assert!(j.script.contains("'mydb'"));
        assert_eq!(j.sshpass, "ftppass"); // 비번은 argv 가 아닌 SSHPASS 로 전달
    }

    #[test]
    fn single_quote_escaped() {
        std::env::set_var("HOME", std::env::temp_dir());
        let j = build(OpKind::DbBackup, "omg", "ex.com", &sample_site(), &Site::default(), None, false).unwrap();
        // db'pa ss → '\'' 이스케이프 포함
        assert!(j.script.contains("'\\''"));
    }

    #[test]
    fn file_backup_has_rsync_and_tar_fallback() {
        std::env::set_var("HOME", std::env::temp_dir());
        let j = build(OpKind::FileBackup, "omg", "ex.com", &sample_site(), &Site::default(), None, false).unwrap();
        assert!(j.script.contains("rsync"), "rsync 우선 시도 없음");
        assert!(j.script.contains("tar -C"), "tar 폴백 없음");
        assert!(j.script.contains("xzf -"), "tar 추출 없음");
    }

    #[test]
    fn file_backup_requires_path() {
        std::env::set_var("HOME", std::env::temp_dir());
        let mut s = sample_site();
        s.path.clear();
        assert!(build(OpKind::FileBackup, "omg", "ex.com", &s, &Site::default(), None, false).is_err());
    }

    #[test]
    fn backup_requires_ip() {
        std::env::set_var("HOME", std::env::temp_dir());
        let mut s = sample_site();
        s.ip.clear();
        assert!(build(OpKind::DbBackup, "omg", "ex.com", &s, &Site::default(), None, false).is_err());
    }

    #[test]
    fn test_conn_job_shape() {
        let j = build(OpKind::TestAsis, "omg", "ex.com", &sample_site(), &Site::default(), None, false).unwrap();
        assert!(j.script.contains("sshpass -e"));
        assert!(j.script.contains("command -v"), "원격 도구 확인 누락");
        assert_eq!(j.sshpass, "ftppass");
    }

    #[test]
    fn db_direct_splits_sshpass_env() {
        std::env::set_var("HOME", std::env::temp_dir());
        let mut tobe = sample_site();
        tobe.ip = "5.6.7.8".into();
        tobe.ftp_pw = "tobepw".into();
        let j = build(OpKind::DbDirect, "omg", "ex.com", &sample_site(), &tobe, None, false).unwrap();
        assert!(j.script.contains("mysqldump"));
        assert!(j.script.contains("HM_ASIS") && j.script.contains("HM_TOBE"));
        assert_eq!(j.sshpass, "", "직접 이전은 단일 SSHPASS 미사용");
        assert!(j.env.iter().any(|(k, v)| k == "HM_ASIS" && v == "ftppass"));
        assert!(j.env.iter().any(|(k, v)| k == "HM_TOBE" && v == "tobepw"));
    }

    #[test]
    fn file_direct_tar_pipe() {
        std::env::set_var("HOME", std::env::temp_dir());
        let j = build(OpKind::FileDirect, "omg", "ex.com", &sample_site(), &sample_site(), None, false).unwrap();
        assert!(j.script.contains("tar -C"));
        assert!(j.script.contains("czf -") && j.script.contains("xzf -"));
        assert!(j.env.iter().any(|(k, _)| k == "HM_TOBE"));
    }

    #[test]
    fn generated_scripts_are_valid_bash() {
        std::env::set_var("HOME", std::env::temp_dir());
        // 따옴표 중첩(bash -lc + 이중 single-quote)이 구문상 유효한지 검사
        for kind in [
            OpKind::DbBackup,
            OpKind::FileBackup,
            OpKind::DbDirect,
            OpKind::FileDirect,
            OpKind::TestAsis,
            OpKind::TestTobe,
            OpKind::CertTobe,
            OpKind::VerifyTobe,
            OpKind::FixHtaccessTobe,
            OpKind::SetDbTobe,
        ] {
            let j = build(kind, "omg", "ex.com", &sample_site(), &sample_site(), None, false).unwrap();
            let out = std::process::Command::new("bash")
                .args(["-n", "-c", &j.script])
                .output()
                .expect("bash 실행 실패");
            assert!(
                out.status.success(),
                "bash 구문 오류 ({}):\n{}\n--- script ---\n{}",
                j.title,
                String::from_utf8_lossy(&out.stderr),
                j.script,
            );
        }
    }

    #[test]
    fn parse_wp_config_db_info() {
        let text = "found: /var/www/html/wp-config.php\n\
                    define( 'DB_NAME', 'sampledb' );\n\
                    define( 'DB_USER', 'sampleuser' );\n\
                    define('DB_PASSWORD', 'Sample-Pass123');\n\
                    define( 'DB_HOST', '127.0.0.1' );\n";
        let d = parse_detected(text);
        assert_eq!(d.name.as_deref(), Some("sampledb"));
        assert_eq!(d.user.as_deref(), Some("sampleuser"));
        assert_eq!(d.pass.as_deref(), Some("Sample-Pass123"));
        assert_eq!(d.host.as_deref(), Some("127.0.0.1"));
    }

    #[test]
    fn parse_rhymix_config() {
        let text = "$config['db']['master']['host'] = 'localhost';\n\
                    $config['db']['master']['user'] = 'rxuser';\n\
                    $config['db']['master']['pass'] = 'rxpass';\n\
                    $config['db']['master']['dbname'] = 'rxdb';\n";
        let d = parse_detected(text);
        assert_eq!(d.host.as_deref(), Some("localhost"));
        assert_eq!(d.user.as_deref(), Some("rxuser"));
        assert_eq!(d.pass.as_deref(), Some("rxpass"));
        assert_eq!(d.name.as_deref(), Some("rxdb"));
        assert_eq!(d.cms.as_deref(), Some("Rhymix"));
    }

    #[test]
    fn parse_xe_config() {
        let text = "'db_hostname' => 'localhost',\n'db_userid' => 'xeuser',\n\
                    'db_password' => 'xepass',\n'db_database' => 'xedb',\n";
        let d = parse_detected(text);
        assert_eq!(d.host.as_deref(), Some("localhost"));
        assert_eq!(d.user.as_deref(), Some("xeuser"));
        assert_eq!(d.name.as_deref(), Some("xedb"));
        assert_eq!(d.cms.as_deref(), Some("XE"));
    }

    #[test]
    fn parse_gnuboard5_config() {
        let text = "define('G5_MYSQL_HOST', 'localhost');\ndefine('G5_MYSQL_USER', 'gbuser');\n\
                    define('G5_MYSQL_PASSWORD', 'gbpass');\ndefine('G5_MYSQL_DB', 'gbdb');\n";
        let d = parse_detected(text);
        assert_eq!(d.host.as_deref(), Some("localhost"));
        assert_eq!(d.name.as_deref(), Some("gbdb"));
        assert_eq!(d.cms.as_deref(), Some("그누보드5"));
    }

    #[test]
    fn parse_gnuboard4_config() {
        let text = "$mysql_host = 'localhost';\n$mysql_user = 'g4user';\n\
                    $mysql_password = 'g4pass';\n$mysql_db = 'g4db';\n";
        let d = parse_detected(text);
        assert_eq!(d.host.as_deref(), Some("localhost"));
        assert_eq!(d.user.as_deref(), Some("g4user"));
        assert_eq!(d.name.as_deref(), Some("g4db"));
        assert_eq!(d.cms.as_deref(), Some("그누보드4"));
    }

    #[test]
    fn eondcms_jobs_valid_bash() {
        let mut server = sample_site();
        server.root_id = "tong".into();
        server.root_pw = "tongpw".into();
        // sudo=false(직접 root), sudo=true(tong+sudo bash -s) 둘 다 구문 검증
        for sudo in [false, true] {
            let e = EondInstall {
                sudo,
                hestia_user: "jokbo".into(),
                hestia_pass: "upass".into(),
                hestia_email: "a@b.c".into(),
                port: "8002".into(),
                db_name: "eondcms".into(),
                db_user: "eondcms".into(),
                db_pass: "db'p ass".into(),
                admin_pass: "adminpw".into(),
                code_local: "/home/dell/dev/eondcms".into(),
                ..Default::default()
            };
            let jobs = [
                build_eondcms_resources(&server, &e, "예시도메인.com", true).unwrap(),
                build_eondcms_upload(&server, &e, "예시도메인.com", true).unwrap(),
                build_eondcms_finalize(&server, &e, "예시도메인.com", true).unwrap(),
                build_eondcms_update(&server, &e, "예시도메인.com", true).unwrap(),
            ];
            for job in jobs {
                let out = std::process::Command::new("bash")
                    .args(["-n", "-c", &job.script])
                    .output()
                    .expect("bash");
                assert!(
                    out.status.success(),
                    "bash 구문 오류 (sudo={sudo}, {}): {}",
                    job.title,
                    String::from_utf8_lossy(&out.stderr)
                );
            }
        }
    }

    #[test]
    fn eondcms_install_needs_root() {
        let server = sample_site(); // root 없음
        let e = EondInstall { hestia_user: "jokbo".into(), ..Default::default() };
        assert!(build_eondcms_resources(&server, &e, "ex.com", false).is_err());
    }

    #[test]
    fn wordpress_jobs_valid_bash() {
        let mut server = sample_site();
        server.root_id = "tong".into();
        server.root_pw = "tongpw".into();
        for sudo in [false, true] {
            let c = CmsInstall {
                kind: CmsKind::WordPress,
                sudo,
                hestia_user: "wpuser".into(),
                hestia_pass: "upass".into(),
                hestia_email: "a@b.c".into(),
                db_name: "wpuser_wp".into(),
                db_user: "wpuser_wp".into(),
                db_pass: "db'p ass".into(),
                admin_user: "admin".into(),
                admin_pass: "adminpw".into(),
                admin_email: "admin@b.c".into(),
                site_title: "My Site".into(),
                ..Default::default()
            };
            let rhymix = CmsInstall { kind: CmsKind::Rhymix, ..c.clone() };
            let gnu = CmsInstall { kind: CmsKind::Gnuboard, ..c.clone() };
            for job in [
                build_cms_install(&server, &c, "예시도메인.com", true).unwrap(),
                build_cms_update(&server, &c, "예시도메인.com", true).unwrap(),
                build_cms_install(&server, &rhymix, "예시도메인.com", true).unwrap(),
                build_cms_update(&server, &rhymix, "예시도메인.com", true).unwrap(),
                build_cms_install(&server, &gnu, "예시도메인.com", true).unwrap(),
                build_cms_update(&server, &gnu, "예시도메인.com", true).unwrap(),
            ] {
                let out = std::process::Command::new("bash")
                    .args(["-n", "-c", &job.script])
                    .output()
                    .expect("bash");
                assert!(
                    out.status.success(),
                    "bash 구문 오류 (sudo={sudo}, {}): {}",
                    job.title,
                    String::from_utf8_lossy(&out.stderr)
                );
            }
        }
    }

    #[test]
    fn cms_install_needs_root_and_wp_admin() {
        let mut server = sample_site();
        server.root_id = "tong".into();
        server.root_pw = "tongpw".into();
        // root 꺼짐 → 에러
        let c = CmsInstall { hestia_user: "u".into(), db_name: "u_wp".into(), db_user: "u_wp".into(), db_pass: "p".into(), ..Default::default() };
        assert!(build_cms_install(&sample_site(), &c, "ex.com", false).is_err());
        // WordPress 설치인데 admin 이메일/비번 없음 → 에러
        assert!(build_cms_install(&server, &c, "ex.com", true).is_err());
        // Rhymix/그누보드는 admin 불필요(마법사) — DB만 있으면 Ok
        let r = CmsInstall { kind: CmsKind::Rhymix, hestia_user: "u".into(), db_name: "u_d".into(), db_user: "u_d".into(), db_pass: "p".into(), ..Default::default() };
        assert!(build_cms_install(&server, &r, "ex.com", true).is_ok());
        let gn = CmsInstall { kind: CmsKind::Gnuboard, hestia_user: "u".into(), db_name: "u_d".into(), db_user: "u_d".into(), db_pass: "p".into(), ..Default::default() };
        assert!(build_cms_install(&server, &gn, "ex.com", true).is_ok());
    }

    #[test]
    fn punycode_korean_domain() {
        let out = idna::domain_to_ascii("한국.kr").unwrap();
        assert!(out.starts_with("xn--"), "got {out}");
        assert!(out.ends_with(".kr"), "got {out}");
    }
}
