//! 백업/복원 작업: 쉘 명령 생성 + 백그라운드 실행 + 로그 스트리밍.
//!
//! 입력은 FTP 계정으로 받지만 백업은 mysqldump/rsync(=SSH) 로 수행하므로,
//! FTP id/pw 를 SSH 로그인으로 사용한다 (대부분의 공유호스팅에서 동일).

use crate::model::{Settings, Site};
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

/// mysqldump 명령(원격 실행용) 생성.
/// MariaDB 의 mysqldump 는 `--set-gtid-purged` 를 모른다(`unknown variable 'set-gtid-purged=OFF'` 로 즉시 실패).
/// 이 옵션은 MySQL 전용이므로, 원격에서 `mysqldump --help` 로 지원 여부를 확인해 지원할 때(=MySQL)만 붙인다.
fn mysqldump_cmd(site: &Site) -> String {
    format!(
        "GTID=$(mysqldump --help 2>/dev/null | grep -q set-gtid-purged && printf %s ' --set-gtid-purged=OFF'); \
         mysqldump {conn} -u {user}{pass} --single-transaction${{GTID}} --routines --triggers --default-character-set=utf8mb4 {db}",
        conn = mysql_conn(site.db_host_or_default(), site.db_port_or_default()),
        user = sq(site.db_id.trim()),
        pass = mysql_pass_arg(site.db_pw.trim()),
        db = sq(site.db_name.trim()),
    )
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

/// 특정 유저·도메인의 alias 목록 (HestiaCP API)
pub fn list_aliases(s: &Settings, user: &str, domain: &str) -> Result<Vec<String>, String> {
    let da = to_ascii_domain(domain);
    let (script, env) = hestia_api_job(s, "v-list-web-domain", &[user.trim(), &da, "json"]);
    let out = run_capture(&script, &env)?;
    let v = parse_json(&out)?;
    let obj = v.as_object().ok_or("도메인 정보 형식 오류")?;
    // { "<domain>": { "ALIAS": "www.x,y", ... } }
    let alias = obj.values().next()
        .and_then(|d| d.get("ALIAS"))
        .and_then(|a| a.as_str())
        .unwrap_or("");
    let list: Vec<String> = alias.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty() && x != &da).collect();
    Ok(list)
}

/// 특정 유저의 웹도메인별 생성일 (도메인, DATE=YYYY-MM-DD). HestiaCP web.conf 기준(v-list-web-domains).
pub fn hestia_web_domain_dates(s: &Settings, user: &str) -> Result<Vec<(String, String)>, String> {
    let (script, env) = hestia_api_job(s, "v-list-web-domains", &[user, "json"]);
    let out = run_capture(&script, &env)?;
    let v = parse_json(&out)?;
    let obj = v.as_object().ok_or("도메인 목록 형식 오류")?;
    let mut res = Vec::new();
    for (dom, fields) in obj {
        let date = fields.get("DATE").and_then(|x| x.as_str()).unwrap_or("").to_string();
        res.push((dom.clone(), date));
    }
    Ok(res)
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

// ===== HestiaCP 패널 진단 (웹패널 8083 접속 불가 원인 수집) =====

/// 비-제로 종료여도 stdout+stderr 를 모아 돌려주는 관용(lax) 러너.
fn run_capture_lax(script: &str) -> Result<String, String> {
    let out = std::process::Command::new("bash")
        .arg("-c").arg(script)
        .output().map_err(|e| format!("실행 실패: {e}"))?;
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    let err = String::from_utf8_lossy(&out.stderr);
    if !err.trim().is_empty() {
        s.push_str("\n[stderr]\n");
        s.push_str(&err);
    }
    Ok(s)
}

/// 패널 네트워크 진단 (로컬 실행, SSH 불필요).
/// 내 PC → 패널(8083) 의 TCP 도달성·HTTP 상태·TLS·인증서 만료를 확인해 보고서 반환.
pub fn hestia_panel_probe(s: &Settings) -> Result<String, String> {
    let host = s.hestia_host.trim();
    if host.is_empty() {
        return Err("HestiaCP 호스트가 비어 있습니다 (설정 > HestiaCP 연동)".into());
    }
    let port = s.port_or_default().to_string();
    let url = format!("https://{host}:{port}/login/");
    let script = format!(
        "set +e\n\
         echo \"=== 패널 네트워크 진단 (내 PC → {host}:{port}) ===\"\n\
         echo \"[1] TCP 연결\"\n\
         if (exec 3<>/dev/tcp/{host}/{port}) 2>/dev/null; then echo \"  OK  {port} 포트 TCP 연결됨\"; else echo \"  실패  {port} 포트 TCP 연결 안 됨 → hestia 서비스 중단 또는 방화벽\"; fi\n\
         echo \"[2] HTTP/TLS 응답\"\n\
         curl -k -sS -o /dev/null -w '  HTTP상태=%{{http_code}}  연결=%{{time_connect}}s  TLS=%{{time_appconnect}}s  총=%{{time_total}}s\\n' --max-time 15 {urlq} 2>&1\n\
         echo \"  curl종료코드=$?  (7=연결거부 28=타임아웃 35/60=TLS)\"\n\
         echo \"[3] 인증서 유효기간\"\n\
         echo | openssl s_client -connect {host}:{port} -servername {host} 2>/dev/null | openssl x509 -noout -dates 2>/dev/null || echo \"  (openssl 로 확인 불가)\"\n\
         echo \"=== 끝 ===\"\n",
        host = host, port = port, urlq = sq(&url),
    );
    run_capture_lax(&script)
}

/// 패널 서버 진단 (SSH, sudo 경유, 읽기 전용).
/// hestia 서비스 상태·8083 LISTEN·nginx 설정 문법(패널/웹 양쪽)·에러로그·디스크·인증서를 수집.
/// 서브도메인 alias 추가 등으로 nginx 설정이 깨져 패널이 안 뜨는 경우의 원인 라인을 잡아낸다.
pub fn build_panel_diagnose(s: &Settings) -> Result<Job, String> {
    let host = if s.ssh_host.trim().is_empty() { s.hestia_host.trim() } else { s.ssh_host.trim() };
    if host.is_empty() {
        return Err("서버 SSH 호스트가 비어 있습니다 (설정 > 서버 SSH 또는 HestiaCP 호스트)".into());
    }
    let user = s.ssh_user.trim();
    if user.is_empty() {
        return Err("SSH 유저(sudo 권한, 예: tong)가 비어 있습니다 (설정 > 서버 SSH)".into());
    }
    if s.ssh_pass.is_empty() {
        return Err("SSH 비밀번호가 비어 있습니다 (설정 > 서버 SSH)".into());
    }
    let port = s.port_or_default().to_string();
    // sudo 경유 SSH 재사용을 위해 합성 Site (FTP 계정칸 = sudo 유저)
    let srv = Site {
        ip: host.to_string(),
        ftp_id: user.to_string(),
        ftp_pw: s.ssh_pass.clone(),
        ssh_port: s.ssh_port.trim().to_string(),
        ..Default::default()
    };
    // 단일 인용 heredoc 안에서 원격 root bash 로 실행됨 (로컬 확장 없음, $ 는 원격에서 평가)
    let raw = format!(
        r#"set +e
export PATH="$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin"
PORT={port}
echo "===== HestiaCP 패널 진단 (포트 $PORT) ====="
echo "[1] hestia 서비스 상태"
systemctl status hestia --no-pager -l 2>&1 | head -n 18 || service hestia status 2>&1 | head -n 18
echo
echo "[2] $PORT 포트 LISTEN 여부"
( ss -tlnp 2>/dev/null || netstat -tlnp 2>/dev/null ) | grep -E "[:.]$PORT[[:space:]]" || echo "  LISTEN 없음 → hestia-nginx 미기동 (가장 흔한 원인)"
echo
echo "[3] hestia-nginx / hestia-php 프로세스"
ps aux | grep -E 'hestia-(nginx|php)' | grep -v grep | head || echo "  프로세스 없음"
echo
echo "[4] 패널 nginx 설정 문법검사"
NB=/usr/local/hestia/nginx/sbin/hestia-nginx
[ -x "$NB" ] || NB=/usr/local/hestia/nginx/sbin/nginx
"$NB" -t -c /usr/local/hestia/nginx/conf/nginx.conf 2>&1 | tail -n 8 || echo "  (패널 nginx 바이너리 없음)"
echo
echo "[5] 시스템 웹 nginx 설정 문법검사 (alias 추가가 깨뜨렸을 수 있음)"
if command -v nginx >/dev/null 2>&1; then nginx -t 2>&1 | tail -n 10; else echo "  (시스템 nginx 없음 — apache 일 수 있음)"; fi
echo
echo "[5b] 최근 변경된 vhost nginx conf (방금 추가한 alias 도메인 추적)"
ls -lt /home/*/conf/web/*/nginx.conf* 2>/dev/null | head -n 8 || echo "  (없음)"
echo
echo "[5c] eondcms 커스텀 nginx 템플릿 (alias conf 와 충돌 의심)"
ls -la /usr/local/hestia/data/templates/web/nginx/*.tpl 2>/dev/null | grep -iE 'eond|proxy|node|fastapi|uvicorn' || echo "  (eondcms 계열 템플릿 없음)"
echo
echo "[5d] 중복 server_name (충돌 = reload 실패의 흔한 원인)"
grep -rhoE 'server_name[^;]+;' /home/*/conf/web/*/nginx.conf* 2>/dev/null | sort | uniq -d | head || echo "  (중복 없음/확인불가)"
echo
echo "[6] 패널/웹 에러로그 (마지막 20줄씩)"
for L in /usr/local/hestia/log/nginx-error.log /var/log/hestia/nginx-error.log /var/log/hestia/error.log /var/log/nginx/error.log; do
  [ -f "$L" ] && {{ echo "--- $L ---"; tail -n 20 "$L"; }}
done
echo
echo "[7] 디스크 / inode (가득 차면 패널 기동 실패)"
df -h / 2>&1; echo; df -i / 2>&1
echo
echo "[8] 패널 SSL 인증서 만료"
echo | openssl s_client -connect 127.0.0.1:$PORT 2>/dev/null | openssl x509 -noout -dates 2>/dev/null || echo "  (확인 불가)"
echo
echo "[9] 서버 자체에서 패널 응답(127.0.0.1)"
curl -k -sS -o /dev/null -w '  HTTP=%{{http_code}}  총=%{{time_total}}s\n' --max-time 10 https://127.0.0.1:$PORT/ 2>&1 || echo "  로컬 응답 없음"
echo
echo "[10] 오래 걸리는 통계/도우미 프로세스 (php-fpm 워커 점유 → 백엔드 행 유발)"
ps -eo pid,etimes,cmd --sort=-etimes 2>/dev/null | grep -E 'awstats\.pl|v-update-web-domain-stat|v-change-web-domain-stats|webalizer' | grep -v grep | awk '{{ tag=($2>120?"  멈춤의심":"  "); print tag" "$0 }}' || true
echo "    (위에 etimes(2번째 열) 큰 항목이 있으면 그게 패널을 막는 원인)"
echo "[11] 패널 php-fpm 워커 수"
ps --no-headers -C php-fpm 2>/dev/null | grep -c 'hestia' 2>/dev/null; ps -eo cmd 2>/dev/null | grep -c '[h]estia/php' || true
echo
echo "===== 진단 끝 ====="
echo "※ [2] LISTEN 없음 + [1] inactive/failed  → sudo systemctl restart hestia"
echo "※ [2] LISTEN 있음 + [9] 타임아웃 + [10] 멈춘 프로세스  → 백엔드 행: 통계 프로세스 정리 후 hestia 재시작 (hostmover '패널 복구' 버튼)"
echo "※ [4]/[5] 에서 설정 오류 라인이 보이면  → 해당 도메인 conf 수정 후 v-rebuild-web-domains <유저>, 그 뒤 systemctl restart hestia"
echo "※ [7] Use% 100% 면 디스크 정리 후 재시작"
"#,
        port = port,
    );
    let (script, sshpass, env) = eondcms_exec(&srv, &raw, false, true);
    Ok(Job {
        title: format!("HestiaCP 패널 진단 (포트 {port})"),
        script,
        sshpass,
        env,
        note: format!("{user}@{host} → sudo 로 hestia 서비스/포트/nginx설정/로그/디스크/인증서 수집 (읽기 전용)"),
    })
}

/// 패널 복구 (SSH, sudo, 서버 상태 변경) — 확인 모달 경유 권장.
/// 멈춘 통계/awstats 프로세스(php-fpm 워커 점유)를 정리하고 hestia 를 재시작한 뒤 응답을 확인한다.
/// "포트는 열려 있는데 패널 페이지가 타임아웃" (백엔드 행) 상황의 표준 복구 절차.
pub fn build_panel_recover(s: &Settings) -> Result<Job, String> {
    let host = if s.ssh_host.trim().is_empty() { s.hestia_host.trim() } else { s.ssh_host.trim() };
    if host.is_empty() {
        return Err("서버 SSH 호스트가 비어 있습니다 (설정 > 서버 SSH 또는 HestiaCP 호스트)".into());
    }
    let user = s.ssh_user.trim();
    if user.is_empty() {
        return Err("SSH 유저(sudo 권한, 예: tong)가 비어 있습니다 (설정 > 서버 SSH)".into());
    }
    if s.ssh_pass.is_empty() {
        return Err("SSH 비밀번호가 비어 있습니다 (설정 > 서버 SSH)".into());
    }
    let port = s.port_or_default().to_string();
    let srv = Site {
        ip: host.to_string(),
        ftp_id: user.to_string(),
        ftp_pw: s.ssh_pass.clone(),
        ssh_port: s.ssh_port.trim().to_string(),
        ..Default::default()
    };
    let raw = format!(
        r#"set +e
export PATH="$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin"
PORT={port}
echo "===== HestiaCP 패널 복구 ====="
echo "[1] 멈춘 통계/awstats 프로세스 정리 (안전 — 통계 갱신만 한 번 건너뜀)"
for P in 'awstats\.pl' 'v-update-web-domain-stat' 'v-change-web-domain-stats'; do
  PIDS=$(pgrep -f "$P" 2>/dev/null)
  if [ -n "$PIDS" ]; then echo "  kill [$P]: $PIDS"; pkill -f "$P" 2>/dev/null; fi
done
sleep 2
# 끈질기게 남으면 강제 종료
for P in 'awstats\.pl' 'v-update-web-domain-stat' 'v-change-web-domain-stats'; do
  pkill -9 -f "$P" 2>/dev/null && echo "  강제종료 [$P]" || true
done
echo
echo "[2] hestia 재시작 (php-fpm 워커 초기화)"
systemctl restart hestia 2>&1 || service hestia restart 2>&1
sleep 3
echo
echo "[3] 8083 LISTEN 확인"
( ss -tlnp 2>/dev/null || netstat -tlnp 2>/dev/null ) | grep -E "[:.]$PORT[[:space:]]" || echo "  ✗ 아직 LISTEN 없음"
echo
echo "[4] 패널 응답 확인 (127.0.0.1)"
curl -k -sS -o /dev/null -w '  HTTP=%{{http_code}}  총=%{{time_total}}s\n' --max-time 12 https://127.0.0.1:$PORT/ 2>&1
RC=$?
if [ "$RC" = 0 ]; then echo "  ✓ 패널 응답 정상 — 접속 복구됨"; else echo "  ✗ 아직 응답 없음 (curl=$RC) — 진단을 다시 돌려 [10]/[5] 확인"; fi
echo "===== 복구 끝 ====="
"#,
        port = port,
    );
    let (script, sshpass, env) = eondcms_exec(&srv, &raw, false, true);
    Ok(Job {
        title: format!("HestiaCP 패널 복구 (포트 {port})"),
        script,
        sshpass,
        env,
        note: format!("{user}@{host} → 멈춘 통계 프로세스 정리 + hestia 재시작 (서버 상태 변경)"),
    })
}

// ===== 디스크 건강 진단 (백업/데이터 디스크 손상 사전 감지) =====

/// 설정의 SSH 접속용 합성 Site 를 구성 (sudo 유저). 패널/디스크 진단 공용.
fn server_ssh_site(s: &Settings) -> Result<(Site, String), String> {
    let host = if s.ssh_host.trim().is_empty() { s.hestia_host.trim() } else { s.ssh_host.trim() };
    if host.is_empty() {
        return Err("서버 SSH 호스트가 비어 있습니다 (설정 > 서버 SSH 또는 HestiaCP 호스트)".into());
    }
    let user = s.ssh_user.trim();
    if user.is_empty() {
        return Err("SSH 유저(sudo 권한, 예: tong)가 비어 있습니다 (설정 > 서버 SSH)".into());
    }
    if s.ssh_pass.is_empty() {
        return Err("SSH 비밀번호가 비어 있습니다 (설정 > 서버 SSH)".into());
    }
    let srv = Site {
        ip: host.to_string(),
        ftp_id: user.to_string(),
        ftp_pw: s.ssh_pass.clone(),
        ssh_port: s.ssh_port.trim().to_string(),
        ..Default::default()
    };
    Ok((srv, format!("{user}@{host}")))
}

/// 디스크 건강 종합 진단 (SSH, sudo, 읽기 전용).
/// dmesg I/O·ext4 체크섬 에러, tune2fs FS 에러카운트, SMART, RO 재마운트, df 를 수집한다.
/// ※ 교훈(2026-06): SMART PASSED 여도 silent corruption 가능 — 확정은 build_disk_scrub(write→read-back).
pub fn build_disk_health(s: &Settings) -> Result<Job, String> {
    let (srv, who) = server_ssh_site(s)?;
    let raw = r#"set +e
export PATH="$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin"
echo "===== 디스크 건강 진단 ====="
echo "[1] 마운트된 실디스크 파일시스템 (RO 재마운트 = 손상 신호)"
findmnt -rno SOURCE,TARGET,FSTYPE,OPTIONS -t ext2,ext3,ext4,xfs 2>/dev/null \
  | awk '{ ro=($0 ~ /(^|,)ro(,| )/)?"  <<< 읽기전용(RO)! 손상 의심":""; print "  "$0 ro }' \
  || mount | grep -E 'ext4|xfs'
echo
echo "[2] dmesg — 저수준 I/O / 파일시스템 에러 (있으면 핵심 증거)"
dmesg -T 2>/dev/null | grep -iE 'I/O error|blk_update_request|Medium Error|hard resetting|ata[0-9].*(error|reset)|Buffer I/O|EXT4-fs error|failed CRC|bitmap checksum|Data will be lost|EUCLEAN|EBADMSG|XFS.*(error|corrupt)|remount.*read-only' | tail -n 25
[ $? -ne 0 ] && true
if ! dmesg -T 2>/dev/null | grep -qiE 'I/O error|EXT4-fs error|Medium Error|failed CRC|Data will be lost'; then echo "  (저수준/ext4 에러 없음 — 양호)"; fi
echo
echo "[3] ext4 파일시스템 상태 / 에러카운트 (tune2fs)"
for DEV in $(findmnt -rno SOURCE -t ext2,ext3,ext4 2>/dev/null | sort -u); do
  [ -b "$DEV" ] || continue
  echo "--- $DEV ---"
  tune2fs -l "$DEV" 2>/dev/null | grep -iE 'Filesystem state|FS Error count|First error|Last error|Mount count|Maximum mount|Last checked|Check interval' \
    | sed 's/^/    /'
  ST=$(tune2fs -l "$DEV" 2>/dev/null | awk -F: '/Filesystem state/{gsub(/ /,"",$2);print $2}')
  EC=$(tune2fs -l "$DEV" 2>/dev/null | awk -F: '/FS Error count/{gsub(/ /,"",$2);print $2}')
  [ "$ST" != "clean" ] && [ -n "$ST" ] && echo "    >>> 상태가 clean 아님($ST) — 다음 부팅 시 fsck 필요"
  [ -n "$EC" ] && [ "$EC" -gt 0 ] 2>/dev/null && echo "    >>> 누적 FS 에러 ${EC}건 — 손상 이력 있음(증가하면 진행형)"
done
echo
echo "[4] SMART (참고용 — PASSED 도 silent corruption 배제 못 함)"
if command -v smartctl >/dev/null 2>&1; then
  for D in $(lsblk -dno NAME,TYPE 2>/dev/null | awk '$2=="disk"{print $1}'); do
    echo "--- /dev/$D ---"
    smartctl -H /dev/$D 2>/dev/null | grep -iE 'overall-health|result' | sed 's/^/    /'
    smartctl -A /dev/$D 2>/dev/null | grep -iE 'Reallocated_Sector|Current_Pending|Offline_Uncorrectable|Reported_Uncorrect|UDMA_CRC_Error|Power_On_Hours' | sed 's/^/    /'
  done
else
  echo "  (smartctl 없음 — apt install smartmontools 권장)"
fi
echo
echo "[5] 용량 / inode"
df -h -x tmpfs -x devtmpfs 2>/dev/null | sed 's/^/  /'
echo
df -i -x tmpfs -x devtmpfs 2>/dev/null | sed 's/^/  /'
echo
echo "===== 진단 끝 ====="
echo "※ [2]에 I/O error/ata reset/Medium Error  → 하드웨어(디스크/케이블/포트) 의심"
echo "※ [2]가 'EXT4-fs error ... checksum'만 + SMART PASSED  → 논리손상일 수 있으나, fsck가 무한반복되면 silent corruption(하드웨어). 무결성 검사로 확정."
echo "※ [3] FS 에러카운트가 시간이 지나며 증가  → 진행형 손상. 즉시 쓰기 중단 + 점검."
echo "※ 확정 판별은 '무결성 검사(write→read-back)' 버튼 — 갓 쓴 데이터가 읽을 때 바뀌면 디스크 교체."
"#;
    let (script, sshpass, env) = eondcms_exec(&srv, raw, false, true);
    Ok(Job {
        title: "디스크 건강 진단".into(),
        script,
        sshpass,
        env,
        note: format!("{who} → dmesg/tune2fs/SMART/df 로 디스크 손상 신호 수집 (읽기 전용)"),
    })
}

/// 디스크 무결성 검사 (write→read-back). SMART/읽기테스트가 못 잡는 silent corruption 확정용.
/// 지정 경로에 임시 파일을 쓰고 sync→캐시드롭→다시 읽어 해시를 비교. 다르면 디스크가 데이터를
/// 정상 저장 못 하는 것(=교체). 임시 파일은 끝나면 삭제. timeout 으로 버스탈락(행)도 잡는다.
pub fn build_disk_scrub(s: &Settings, path: &str, size_mb: u32) -> Result<Job, String> {
    let (srv, who) = server_ssh_site(s)?;
    let p = path.trim();
    if p.is_empty() || !p.starts_with('/') {
        return Err("검사 경로는 / 로 시작하는 절대경로여야 합니다 (예: /backup)".into());
    }
    if p.contains('\'') || p.contains("..") {
        return Err("경로에 허용되지 않는 문자가 있습니다".into());
    }
    let mb = size_mb.clamp(16, 8192);
    // p 는 단일 인용 heredoc 안에서 원격 평가되므로 작은따옴표만 막으면 안전. 원격에서 sq 처리.
    let raw = format!(
        r#"set +e
export PATH="$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin"
DIR='{p}'
MB={mb}
echo "===== 디스크 무결성 검사 (write→read-back) ====="
echo "대상: $DIR  크기: ${{MB}}MB"
if [ ! -d "$DIR" ]; then echo "  ✗ 경로 없음: $DIR"; echo "FAIL"; exit 1; fi
F="$DIR/.hm_scrub_$$.bin"
echo "[1] 쓰기 (urandom → 파일, fsync)"
timeout 300 dd if=/dev/urandom of="$F" bs=1M count="$MB" conv=fsync status=none
RC=$?
if [ "$RC" = 124 ]; then echo "  ✗ 쓰기 타임아웃 — 디스크가 부하 중 응답 없음(버스 탈락 의심)"; rm -f "$F" 2>/dev/null; echo "FAIL"; exit 1; fi
if [ "$RC" != 0 ]; then echo "  ✗ 쓰기 실패(dd=$RC) — RO 재마운트/공간부족/장치오류"; rm -f "$F" 2>/dev/null; echo "FAIL"; exit 1; fi
sync
H1=$(timeout 300 sha256sum "$F" 2>/dev/null | awk '{{print $1}}')
echo "[2] 캐시 드롭 (디스크에서 강제 재독)"
sync; echo 3 > /proc/sys/vm/drop_caches 2>/dev/null || echo "  (drop_caches 불가 — 캐시 영향 가능)"
echo "[3] 재독 후 해시 비교"
H2=$(timeout 300 sha256sum "$F" 2>/dev/null | awk '{{print $1}}')
RC=$?
rm -f "$F" 2>/dev/null
if [ "$RC" = 124 ]; then echo "  ✗ 재독 타임아웃 — 부하 시 장치 탈락(하드웨어 결함 강력 의심)"; echo "FAIL"; exit 1; fi
echo "    쓰기직후: $H1"
echo "    재독후  : $H2"
if [ -z "$H1" ] || [ -z "$H2" ]; then echo "  ✗ 해시 계산 실패(읽기 오류)"; echo "FAIL"; exit 1; fi
if [ "$H1" = "$H2" ]; then
  echo "  ✓ 일치 — 이 크기/구간에서는 정상 저장됨 (silent corruption 미검출)"
  echo "    ※ 간헐적 손상은 더 큰 크기로 반복하면 드러날 수 있음."
  echo "OK"
else
  echo "  ✗✗ 불일치 — 갓 쓴 데이터가 읽을 때 바뀜 = SILENT CORRUPTION. 디스크 교체 필요."
  echo "FAIL"
fi
echo "===== 검사 끝 ====="
"#,
        p = p, mb = mb,
    );
    let (script, sshpass, env) = eondcms_exec(&srv, &raw, false, true);
    Ok(Job {
        title: format!("디스크 무결성 검사 ({p}, {mb}MB)"),
        script,
        sshpass,
        env,
        note: format!("{who} → {p} 에 임시파일 쓰기→캐시드롭→재독 해시비교 (임시파일 자동삭제)"),
    })
}

/// 디스크 자동 감시 설치 (SSH, sudo, 서버 상태 변경 — 확인 모달 권장).
/// smartd 활성화 + 매일 FS에러카운트/dmesg 감시 + 주간 write→read-back 스크럽 + 이상 시 이메일.
pub fn build_disk_monitor_install(s: &Settings, email: &str, scrub_path: &str) -> Result<Job, String> {
    let (srv, who) = server_ssh_site(s)?;
    let em = email.trim();
    if em.is_empty() || !em.contains('@') || em.contains('\'') || em.contains(char::is_whitespace) {
        return Err("알림 이메일 형식이 올바르지 않습니다".into());
    }
    let p = scrub_path.trim();
    if p.is_empty() || !p.starts_with('/') || p.contains('\'') || p.contains("..") {
        return Err("스크럽 경로는 / 로 시작하는 절대경로여야 합니다 (예: /backup)".into());
    }
    // head: 원격 셸 변수로 이메일/경로 주입 (sed/스마트디/요약에서 사용). 스크립트 파일에는 placeholder→sed 로 박음.
    let head = format!("set +e\nEMAIL={}\nSCRUB_PATH={}\n", sq(em), sq(p));
    let body = DISK_MONITOR_INSTALL_BODY;
    let raw = head + body;
    let (script, sshpass, env) = eondcms_exec(&srv, &raw, false, true);
    Ok(Job {
        title: "디스크 자동 감시 설치".into(),
        script,
        sshpass,
        env,
        note: format!("{who} → smartd + 매일 감시 + 주간 스크럽({p}) + 이메일({em}) 설치 (서버 상태 변경)"),
    })
}

/// 디스크 자동 감시 제거 (SSH, sudo).
pub fn build_disk_monitor_uninstall(s: &Settings) -> Result<Job, String> {
    let (srv, who) = server_ssh_site(s)?;
    let raw = r#"set +e
export PATH="$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin"
echo "===== 디스크 자동 감시 제거 ====="
rm -f /etc/cron.d/hm-disk-monitor /usr/local/sbin/hm-disk-monitor.sh /usr/local/sbin/hm-disk-scrub.sh
echo "  cron/스크립트 제거"
if [ -f /etc/smartd.conf ]; then
  sed -i '/# hostmover/d' /etc/smartd.conf
  sed -i 's/^#DEVICESCAN(hm-disabled)/DEVICESCAN/' /etc/smartd.conf
  echo "  smartd.conf 원복(smartd 자체는 유지)"
fi
systemctl restart cron 2>/dev/null || service cron restart 2>/dev/null || systemctl restart crond 2>/dev/null || true
echo "  ※ 상태파일(/var/lib/hm-disk-monitor)·로그(/var/log/hm-disk-monitor.log)는 보존됨"
echo "===== 제거 끝 ====="
"#;
    let (script, sshpass, env) = eondcms_exec(&srv, raw, false, true);
    Ok(Job {
        title: "디스크 자동 감시 제거".into(),
        script,
        sshpass,
        env,
        note: format!("{who} → cron/스크립트 제거 + smartd.conf 원복"),
    })
}

/// 설치 스크립트 본문(셸). format! 비사용 → 셸 중괄호 이스케이프 불필요.
/// 단일 인용 heredoc 안에서 원격 root bash 로 실행됨. 내부 파일 생성도 단일인용 heredoc(EOS)+sed 치환.
const DISK_MONITOR_INSTALL_BODY: &str = r#"
export PATH="$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin"
echo "===== hostmover 디스크 자동 감시 설치 ====="
echo "알림 메일: $EMAIL   주간 스크럽 경로: $SCRUB_PATH"
mkdir -p /var/lib/hm-disk-monitor

# 1) smartmontools
if ! command -v smartctl >/dev/null 2>&1; then
  echo "[설치] smartmontools"
  (apt-get update -qq && apt-get install -y -qq smartmontools) 2>&1 | tail -n 3 \
    || (yum install -y smartmontools 2>&1 | tail -n 3) || echo "  ※ 자동설치 실패 — 수동 설치 필요"
fi

# 2) 매일 감시 스크립트
cat > /usr/local/sbin/hm-disk-monitor.sh <<'EOS'
#!/usr/bin/env bash
export PATH="$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin"
EMAIL="__EMAIL__"
STATE=/var/lib/hm-disk-monitor
LOG=/var/log/hm-disk-monitor.log
ALERT=""
add(){ ALERT="$ALERT$1"$'\n'; echo "$(date '+%F %T') $1" >> "$LOG"; }
for DEV in $(findmnt -rno SOURCE -t ext2,ext3,ext4 2>/dev/null | sort -u); do
  [ -b "$DEV" ] || continue
  ST=$(tune2fs -l "$DEV" 2>/dev/null | awk -F: '/Filesystem state/{gsub(/ /,"",$2);print $2}')
  EC=$(tune2fs -l "$DEV" 2>/dev/null | awk -F: '/FS Error count/{gsub(/ /,"",$2);print $2}'); [ -z "$EC" ] && EC=0
  KEY=$(echo "$DEV" | tr '/' '_'); PREV=$(cat "$STATE/$KEY.count" 2>/dev/null || echo 0); echo "$EC" > "$STATE/$KEY.count"
  [ "$ST" != "clean" ] && [ -n "$ST" ] && add "[$DEV] 파일시스템 상태=$ST (clean 아님)"
  [ "$EC" -gt "$PREV" ] 2>/dev/null && add "[$DEV] FS 에러카운트 증가: $PREV -> $EC (진행형 손상 의심)"
done
DC=$(dmesg 2>/dev/null | grep -icE 'I/O error|Medium Error|EXT4-fs error|failed CRC|bitmap checksum|Data will be lost|hard resetting')
PREV=$(cat "$STATE/dmesg.count" 2>/dev/null || echo 0); echo "$DC" > "$STATE/dmesg.count"
if [ "$DC" -gt "$PREV" ] 2>/dev/null; then
  add "dmesg I/O/FS 에러 증가: $PREV -> $DC"
  dmesg 2>/dev/null | grep -iE 'I/O error|Medium Error|EXT4-fs error|failed CRC|Data will be lost|hard resetting' | tail -n 8 >> "$LOG"
fi
if command -v smartctl >/dev/null 2>&1; then
  for D in $(lsblk -dno NAME,TYPE 2>/dev/null | awk '$2=="disk"{print $1}'); do
    smartctl -H /dev/$D 2>/dev/null | grep -iE 'overall-health' | grep -qiE 'PASSED' || add "[/dev/$D] SMART overall-health PASSED 아님"
    RS=$(smartctl -A /dev/$D 2>/dev/null | awk '/Reallocated_Sector_Ct/{print $10}')
    PS=$(smartctl -A /dev/$D 2>/dev/null | awk '/Current_Pending_Sector/{print $10}')
    [ -n "$RS" ] && [ "$RS" -gt 0 ] 2>/dev/null && add "[/dev/$D] Reallocated_Sector=$RS"
    [ -n "$PS" ] && [ "$PS" -gt 0 ] 2>/dev/null && add "[/dev/$D] Current_Pending_Sector=$PS"
  done
fi
while read -r FS SZ USED AVAIL PCT MP; do
  P=${PCT%\%}; [ "$P" -ge 95 ] 2>/dev/null && add "[$MP] 사용량 $PCT (디스크 거의 참)"
done < <(df -P -x tmpfs -x devtmpfs 2>/dev/null | awk 'NR>1')
if [ -n "$ALERT" ]; then
  SUBJ="[hostmover] 디스크 경보: $(hostname)"
  BODY="디스크 감시에서 이상이 감지되었습니다:"$'\n\n'"$ALERT"$'\n'"로그: $LOG"
  if command -v mail >/dev/null 2>&1; then printf '%s\n' "$BODY" | mail -s "$SUBJ" "$EMAIL"
  elif command -v sendmail >/dev/null 2>&1; then printf 'To: %s\nSubject: %s\n\n%s\n' "$EMAIL" "$SUBJ" "$BODY" | sendmail -t; fi
fi
EOS
sed -i "s|__EMAIL__|$EMAIL|g" /usr/local/sbin/hm-disk-monitor.sh
chmod +x /usr/local/sbin/hm-disk-monitor.sh

# 3) 주간 무결성 스크럽 스크립트
cat > /usr/local/sbin/hm-disk-scrub.sh <<'EOS'
#!/usr/bin/env bash
export PATH="$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin"
EMAIL="__EMAIL__"; DIR="__PATH__"; MB=2048; LOG=/var/log/hm-disk-monitor.log
[ -d "$DIR" ] || { echo "$(date '+%F %T') scrub: 경로 없음 $DIR" >> "$LOG"; exit 0; }
F="$DIR/.hm_scrub_$$.bin"; MSG=""
timeout 600 dd if=/dev/urandom of="$F" bs=1M count="$MB" conv=fsync status=none; RC=$?
if [ "$RC" = 124 ]; then MSG="쓰기 타임아웃 @ $DIR (부하 시 장치 탈락 의심)"; rm -f "$F"
elif [ "$RC" != 0 ]; then MSG="쓰기 실패(dd=$RC) @ $DIR (RO재마운트/공간/장치오류)"; rm -f "$F"
else
  sync; H1=$(sha256sum "$F" | awk '{print $1}')
  sync; echo 3 > /proc/sys/vm/drop_caches 2>/dev/null
  H2=$(sha256sum "$F" | awk '{print $1}'); rm -f "$F"
  [ "$H1" != "$H2" ] && MSG="SILENT CORRUPTION! 갓 쓴 데이터가 읽을 때 바뀜 @ $DIR. 디스크 교체 필요. ($H1 vs $H2)"
fi
echo "$(date '+%F %T') scrub $DIR: ${MSG:-OK}" >> "$LOG"
if [ -n "$MSG" ]; then
  SUBJ="[hostmover] 디스크 무결성 경보: $(hostname)"
  if command -v mail >/dev/null 2>&1; then printf '%s\n' "$MSG" | mail -s "$SUBJ" "$EMAIL"
  elif command -v sendmail >/dev/null 2>&1; then printf 'To: %s\nSubject: %s\n\n%s\n' "$EMAIL" "$SUBJ" "$MSG" | sendmail -t; fi
fi
EOS
sed -i "s|__EMAIL__|$EMAIL|g; s|__PATH__|$SCRUB_PATH|g" /usr/local/sbin/hm-disk-scrub.sh
chmod +x /usr/local/sbin/hm-disk-scrub.sh

# 4) cron.d
cat > /etc/cron.d/hm-disk-monitor <<'EOS'
# hostmover 디스크 감시 (자동 생성)
SHELL=/bin/bash
PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
30 7 * * * root /usr/local/sbin/hm-disk-monitor.sh
10 4 * * 0 root /usr/local/sbin/hm-disk-scrub.sh
EOS
chmod 644 /etc/cron.d/hm-disk-monitor

# 5) smartd
if [ -f /etc/smartd.conf ]; then
  if ! grep -q '# hostmover' /etc/smartd.conf; then
    sed -i 's/^DEVICESCAN/#DEVICESCAN(hm-disabled)/' /etc/smartd.conf
    echo "DEVICESCAN -a -o on -S on -s (S/../.././02|L/../../6/03) -m $EMAIL # hostmover" >> /etc/smartd.conf
  fi
  systemctl enable --now smartd 2>/dev/null || systemctl enable --now smartmontools 2>/dev/null || service smartd restart 2>/dev/null || true
fi

# 6) 베이스라인 1회 수집 + cron 재시작
/usr/local/sbin/hm-disk-monitor.sh
systemctl restart cron 2>/dev/null || service cron restart 2>/dev/null || systemctl restart crond 2>/dev/null || true
echo "[완료]"
echo "  매일 07:30  /usr/local/sbin/hm-disk-monitor.sh  (FS에러카운트/dmesg/SMART/용량)"
echo "  매주 일 04:10  /usr/local/sbin/hm-disk-scrub.sh  ($SCRUB_PATH, write->read-back)"
echo "  로그 /var/log/hm-disk-monitor.log   알림메일 $EMAIL"

# 7) 메일 발송 자가진단 — 실제 테스트 메일을 보내 어떤 전송수단이 되는지 즉시 확인
echo "[메일 발송 테스트] $EMAIL 로 테스트 메일 전송 시도 (전송수단 자동 탐지)"
HOST=$(hostname)
MAILOK=""
if command -v mail >/dev/null 2>&1; then
  printf '%s\n' "hostmover 디스크 자동 감시 설치 완료. 이 메일이 도착했다면 알림 경로가 정상입니다. (transport=mail)" \
    | mail -s "[hostmover] 설치 테스트 $HOST" "$EMAIL" 2>/dev/null && MAILOK="mail"
fi
if [ -z "$MAILOK" ] && command -v sendmail >/dev/null 2>&1; then
  printf 'To: %s\nSubject: [hostmover] 설치 테스트 %s\n\nhostmover 디스크 자동 감시 설치 완료. 이 메일이 도착했다면 알림 경로가 정상입니다. (transport=sendmail)\n' "$EMAIL" "$HOST" \
    | sendmail -t 2>/dev/null && MAILOK="sendmail"
fi
if [ -n "$MAILOK" ]; then
  echo "  ✓ 테스트 메일을 큐에 넣었습니다 (transport=$MAILOK). $EMAIL 수신함을 확인하세요."
  echo "    ※ '전송함'은 큐 투입까지만 보장합니다. 메일이 안 오면 서버 MTA(exim/postfix) 큐/스팸함을 점검하세요:  exim -bp  또는  mailq"
else
  echo "  ✗ mail·sendmail 둘 다 없음 — 이 상태로는 알림 메일이 발송되지 않습니다."
  echo "    → MTA/메일러 설치 필요. 데비안/우분투:  apt-get install -y bsd-mailx   (HestiaCP면 exim4 가 보통 이미 설치됨: which sendmail)"
fi
echo "===== 설치 끝 ====="
"#;

// ===== 일반 CMS 설치 (WordPress / Rhymix / 그누보드) =====

fn cms_validate(server: &Site, c: &CmsInstall, use_root: bool, need_db: bool, need_admin: bool) -> Result<(), String> {
    if server.ip.trim().is_empty() { return Err("설치 대상 서버 IP가 비어 있습니다".into()); }
    if !use_root { return Err("CMS 설치는 root 권한 필요 — '루트로 실행'을 켜고 서버루트 계정을 입력하세요".into()); }
    if server.login_id(use_root).is_empty() { return Err("서버 루트 로그인 아이디가 비어 있습니다".into()); }
    if c.hestia_user.trim().is_empty() { return Err("HestiaCP 유저가 비어 있습니다".into()); }
    // 업데이트(git pull/wp-cli)는 DB 자격증명이 필요 없으므로 설치 시에만 검증
    if need_db && (c.db_name.trim().is_empty() || c.db_user.trim().is_empty() || c.db_pass.is_empty()) {
        return Err("DB 정보가 필요합니다 — 정보 탭에서 대상 서버(현재/신규)의 DB이름/ID/비번을 입력하세요".into());
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
pub fn build_cms_update(server: &Site, c: &CmsInstall, s: &Settings, domain_name: &str, use_root: bool) -> Result<Job, String> {
    match c.kind {
        CmsKind::WordPress => build_wp_update(server, c, domain_name, use_root),
        CmsKind::Rhymix => build_git_cms_update(server, c, s, domain_name, use_root, &RHYMIX),
        CmsKind::Gnuboard => build_git_cms_update(server, c, s, domain_name, use_root, &GNUBOARD),
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
    /// 비-git 오버레이 업데이트 시 보존할 경로(사용자 데이터/설정) — rsync --exclude
    preserve: &'static [&'static str],
    /// 헤드리스(무인) 설치 지원 — true 면 procInstall 부트스트랩으로 브라우저 마법사를 건너뛴다 (Rhymix 전용)
    headless: bool,
}

const RHYMIX: GitCms = GitCms {
    name: "Rhymix",
    repo: "https://github.com/rhymix/rhymix.git",
    writable_dir: "files",
    writable_mode: "u+rwX",
    installer_path: "/",
    preserve: &["config", "files"],
    headless: true,
};
const GNUBOARD: GitCms = GitCms {
    name: "그누보드",
    repo: "https://github.com/gnuboard/gnuboard5.git",
    writable_dir: "data",
    writable_mode: "707",
    installer_path: "/install/",
    preserve: &["data"],
    headless: false,
};

/// Rhymix/그누보드 설치 — v-add-* + git clone + 권한 + DB + SSL (+ 브라우저 마법사 안내)
fn build_git_cms_install(server: &Site, c: &CmsInstall, domain_name: &str, use_root: bool, g: &GitCms) -> Result<Job, String> {
    // 헤드리스(Rhymix) 설치는 관리자 계정을 자동 생성하므로 관리자 비번/이메일이 필요하다.
    cms_validate(server, c, use_root, true, g.headless)?;
    let domain = to_ascii_domain(domain_name);
    let head = format!(
        "set -e\nexport PATH=\"$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin\"\nVBIN=/usr/local/hestia/bin\nVUSER={u}\nDOMAIN={d}\nPKG={pkg}\nEMAIL={em}\nUPASS={up}\nDBNAME={dbn}\nDBUSER={dbu}\nDBPASS={dbp}\nCMSNAME={cn}\nREPO={repo}\nWDIR={wd}\nWMODE={wm}\nINSTPATH={ip}\nADMINU={au}\nADMINP={ap}\nADMINE={ae}\nADMINNICK={an}\nPREFIX={pf}\nHEADLESS={hl}\n",
        u = sq(c.hestia_user.trim()), d = sq(&domain), pkg = sq(c.package_or_default()),
        em = sq(c.hestia_email.trim()), up = sq(&c.hestia_pass),
        dbn = sq(c.db_name.trim()), dbu = sq(c.db_user.trim()), dbp = sq(&c.db_pass),
        cn = sq(g.name), repo = sq(g.repo), wd = sq(g.writable_dir), wm = sq(g.writable_mode), ip = sq(g.installer_path),
        au = sq(c.admin_user_or_default()), ap = sq(&c.admin_pass), ae = sq(c.admin_email.trim()),
        an = sq(c.admin_user_or_default()), pf = sq("rx_"), hl = if g.headless { "1" } else { "0" },
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
if [ "$HEADLESS" = 1 ]; then
  if [ -f "$WEBROOT/files/config/config.php" ]; then
    echo "※ 이미 설치됨(files/config/config.php 존재) — 자동설치 건너뜀(데이터 보호)"
  elif ! command -v php >/dev/null 2>&1; then
    echo "✗ php(CLI) 없음 — 자동설치 불가. 브라우저 마법사로 진행: https://$DOMAIN$INSTPATH"
  else
    echo "-- $CMSNAME 무인 설치(procInstall) — 브라우저 마법사 건너뛰기 --"
    cat > "$WEBROOT/hm_install.php" <<'PHPEOF'
<?php
// hostmover 무인설치: Rhymix 프레임워크를 부트스트랩하고 install 모듈의 procInstall 호출.
// 값은 환경변수로 받아 파일에 비밀이 남지 않게 한다. 실행 후 즉시 삭제된다.
// procInstall 은 실패 시 예외를 던지므로 try/catch 로 사유를 STDERR 에 노출한다.
if (PHP_SAPI !== 'cli') { exit(1); }
error_reporting(E_ALL); ini_set('display_errors', 'stderr');
chdir(__DIR__);
// CLI 에는 HTTP 요청이 없어 common/constants.php 가 RX_BASEURL 을 정의하지 못해 Context::init() 이 죽는다.
// constants.php 의 첫 분기(DOCUMENT_ROOT 기반)는 CLI 가드가 없으므로, require 전에 가짜 웹요청 $_SERVER 를
// 심어 RX_BASEURL/기본 URL 이 정상 정의되게 한다. (admin 링크/Config 기본 URL 도 도메인 기준으로 잡힘)
$__host = getenv('HM_DOMAIN') ?: 'localhost';
$_SERVER['DOCUMENT_ROOT']  = __DIR__;
$_SERVER['HTTP_HOST']      = $__host;
$_SERVER['SERVER_NAME']    = $__host;
$_SERVER['SERVER_PORT']    = '443';
$_SERVER['HTTPS']          = 'on';
$_SERVER['REQUEST_METHOD'] = 'GET';
$_SERVER['REQUEST_URI']    = '/';
$_SERVER['SCRIPT_NAME']    = '/index.php';
$_SERVER['PHP_SELF']       = '/index.php';
$_SERVER['SCRIPT_FILENAME']= __DIR__ . '/index.php';
$_SERVER['REMOTE_ADDR']    = '127.0.0.1';
$autoload = __DIR__ . '/common/autoload.php';
if (!file_exists($autoload)) { fwrite(STDERR, "FAIL: autoload 없음 ($autoload)\n"); exit(1); }
require_once $autoload;
if (!defined('RX_BASEURL')) { define('RX_BASEURL', '/'); }  // 안전망
try {
    Context::init();
    if (Context::isInstalled()) { fwrite(STDERR, "FAIL: 이미 설치됨(Context::isInstalled)\n"); exit(1); }
    $config = (object) array(
        'db_type' => 'mysql',
        'db_hostname' => getenv('HM_DB_HOST'),
        'db_port' => (int) getenv('HM_DB_PORT'),
        'db_userid' => getenv('HM_DB_USER'),
        'db_password' => getenv('HM_DB_PASS'),
        'db_database' => getenv('HM_DB_NAME'),
        'db_table_prefix' => getenv('HM_DB_PREFIX'),
        'db_charset' => 'utf8mb4',
        'email_address' => getenv('HM_ADMIN_EMAIL'),
        'password' => getenv('HM_ADMIN_PW'),
        'nick_name' => getenv('HM_ADMIN_NICK'),
        'user_id' => getenv('HM_ADMIN_ID'),
        'use_rewrite' => 'Y',
        'use_ssl' => 'optional',
        'time_zone' => 'Asia/Seoul',
    );
    if (!function_exists('getController')) { fwrite(STDERR, "FAIL: getController 미정의(부트스트랩 실패)\n"); exit(1); }
    $oController = getController('install');
    if (!is_object($oController) || !method_exists($oController, 'procInstall')) {
        fwrite(STDERR, "FAIL: install 컨트롤러/ procInstall 없음\n"); exit(1);
    }
    $output = $oController->procInstall($config);
    if (is_object($output) && method_exists($output, 'toBool') && !$output->toBool()) {
        fwrite(STDERR, 'FAIL: ' . $output->getMessage() . "\n");
        exit(1);
    }
    echo "OK\n";
} catch (\Throwable $e) {
    fwrite(STDERR, 'EXCEPTION: ' . get_class($e) . ': ' . $e->getMessage() . "\n");
    exit(1);
}
PHPEOF
    chown "$VUSER:$VUSER" "$WEBROOT/hm_install.php"
    if sudo -u "$VUSER" env HM_DOMAIN="$DOMAIN" HM_DB_HOST=127.0.0.1 HM_DB_PORT=3306 \
         HM_DB_USER="$DBUSER" HM_DB_PASS="$DBPASS" HM_DB_NAME="$FULLDB" HM_DB_PREFIX="$PREFIX" \
         HM_ADMIN_ID="$ADMINU" HM_ADMIN_PW="$ADMINP" HM_ADMIN_EMAIL="$ADMINE" HM_ADMIN_NICK="$ADMINNICK" \
         php "$WEBROOT/hm_install.php"; then
      rm -f "$WEBROOT/hm_install.php"
      if [ -f "$WEBROOT/files/config/config.php" ]; then
        echo "✓ $CMSNAME 자동 설치 완료 — 브라우저 마법사 없이 바로 사용 가능"
        echo "   사이트: https://$DOMAIN/   관리자: https://$DOMAIN/index.php?module=admin  (아이디 $ADMINU)"
      else
        echo "※ procInstall 은 성공했으나 config.php 미생성 — 권한/경로 확인 후 마법사로 진행: https://$DOMAIN$INSTPATH"
      fi
    else
      rm -f "$WEBROOT/hm_install.php"
      echo "✗ 자동 설치 실패 — 브라우저 마법사로 진행하세요: https://$DOMAIN$INSTPATH"
      echo "   DB 호스트=127.0.0.1  DB이름=$FULLDB  유저=$DBUSER  비번=(입력한 DB비번)  포트=3306"
      echo "   (Rhymix 버전에 따라 procInstall 시그니처가 다를 수 있습니다. 위 STDERR 메시지를 확인하세요.)"
    fi
  fi
else
  echo "▶ 마지막 단계(브라우저): https://$DOMAIN$INSTPATH 접속 → 설치 마법사"
  echo "   DB 호스트=127.0.0.1  DB이름=$FULLDB  유저=$DBUSER  비번=(입력한 DB비번)  포트=3306"
  echo "   그리고 관리자 계정을 마법사에서 생성하세요."
fi
echo "== $CMSNAME 설치 처리 끝 =="
"#;
    let raw = format!("{head}{body}");
    let (script, sshpass, env) = eondcms_exec(server, &raw, use_root, c.sudo);
    Ok(Job {
        title: format!("{} 설치 : {domain_name}", g.name),
        script,
        sshpass,
        env,
        note: if g.headless { "v-add-* + git clone + 권한 + DB + SSL + 무인설치(procInstall)".into() }
              else { "v-add-* + git clone + 권한 + DB + SSL (그누보드 마법사는 브라우저)".into() },
    })
}

/// 업로드/크기계산에서 제외할 개발 부산물 (basename 기준, 어느 깊이든)
const UPLOAD_EXCLUDES: &[&str] = &[
    "node_modules", ".git", ".github", ".idea", ".vscode",
    ".sass-cache", "bower_components", ".DS_Store", "Thumbs.db", "npm-debug.log",
];

/// rsync/du 용 --exclude 플래그 문자열 (뒤에 공백 포함)
fn rsync_exclude_flags() -> String {
    UPLOAD_EXCLUDES.iter().map(|e| format!("--exclude={e} ")).collect()
}

/// find 용 제외 조건 (`-not -path '*/name/*'` …, 뒤에 공백 포함)
fn find_exclude_conds() -> String {
    UPLOAD_EXCLUDES.iter().map(|e| format!("-not -path '*/{e}/*' ")).collect()
}

/// Rhymix 모듈/레이아웃 업로드: 로컬 dev/rx 의 지정 모듈/레이아웃을 대상 사이트 webroot 로 푸시.
/// 로컬 → 원격 /tmp 스테이징 rsync(로그인 유저) → sudo 로 webroot/modules·layouts 에 복사+chown.
/// 이름은 is_safe_name(영숫자/._-)만 허용(경로탈출/주입 방지). 각 항목은 전체 교체(mirror).
/// node_modules/.git 등 개발 부산물은 제외(UPLOAD_EXCLUDES).
pub fn build_rx_upload(server: &Site, c: &CmsInstall, src_base: &str, modules: &[String], layouts: &[String], domain_name: &str, use_root: bool) -> Result<Job, String> {
    cms_validate(server, c, use_root, false, false)?;
    let src = src_base.trim().trim_end_matches('/');
    if src.is_empty() { return Err("Rhymix 소스 경로가 비어 있습니다 (설정 > 서버 SSH 아래 'Rhymix 소스')".into()); }
    if !std::path::Path::new(src).is_dir() { return Err(format!("로컬 Rhymix 소스 폴더가 없습니다: {src}")); }
    // 이름 검증 + 빈값 제거
    let mut mods: Vec<&str> = Vec::new();
    for m in modules { let m = m.trim(); if m.is_empty() { continue; }
        if !is_safe_name(m) { return Err(format!("모듈 이름 형식 오류(영숫자/._- 만): {m}")); } mods.push(m); }
    let mut lays: Vec<&str> = Vec::new();
    for l in layouts { let l = l.trim(); if l.is_empty() { continue; }
        if !is_safe_name(l) { return Err(format!("레이아웃 이름 형식 오류(영숫자/._- 만): {l}")); } lays.push(l); }
    if mods.is_empty() && lays.is_empty() { return Err("업로드할 모듈/레이아웃 이름을 하나 이상 입력하세요".into()); }
    let domain = to_ascii_domain(domain_name);
    let vuser = c.hestia_user.trim();
    if vuser.is_empty() { return Err("HestiaCP 유저가 비어 있습니다".into()); }
    let staging = format!("/tmp/hm-rx-{}", store::sanitize(&domain));
    let webroot = format!("/home/{vuser}/web/{domain}/public_html");
    let sshopt = ssh_e(server);
    let user = server.login_id(use_root);
    let host = server.ip.trim();

    // 1) 로컬 → 원격 /tmp 스테이징 rsync (먼저 스테이징 비움 → 선택 항목만 정확히 올라감)
    let mut rs = String::new();
    rs.push_str("set -e\n");
    rs.push_str(&format!("echo '== Rhymix 업로드 (로컬 → 스테이징): {domain} =='\n"));
    // 직접 sshpass→ssh 호출: ssh 옵션은 분리된 단어여야 함(sq 로 감싸면 sshpass 가 통째를 실행파일명으로 오인).
    rs.push_str(&format!("sshpass -e {ssh} {u}@{h} {rm}\n",
        ssh = sshopt, u = sq(user), h = sq(host), rm = remote_cmd(&format!("rm -rf {}", sq(&staging)))));
    for (kind, names) in [("modules", &mods), ("layouts", &lays)] {
        for n in names.iter() {
            let local = format!("{src}/{kind}/{n}");
            rs.push_str(&format!(
                "[ -d {lq} ] || {{ echo '✗ 로컬 {kind} 없음: {l}'; exit 1; }}\n\
                 echo '↑ {kind}/{n}'\n\
                 sshpass -e rsync -az --delete --mkpath {excl}-e {ssh} {lq}/ {u}@{h}:{st}/{kind}/{n}/\n",
                lq = sq(&local), l = local, kind = kind, n = n, excl = rsync_exclude_flags(),
                ssh = sq(&sshopt), u = sq(user), h = sq(host), st = sq(&staging),
            ));
        }
    }

    // 2) 원격 sudo: 스테이징 → webroot/modules·layouts 복사(전체교체) + chown + 정리
    let remote_raw = format!(
        "set -e\nexport PATH=\"$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin\"\n\
         VUSER={vu}\nDOMAIN={d}\nSTAGING={st}\n\
         WEBROOT=\"/home/$VUSER/web/$DOMAIN/public_html\"\n\
         echo \"== Rhymix 모듈/레이아웃 설치: $DOMAIN (webroot=$WEBROOT) ==\"\n\
         [ -d \"$WEBROOT\" ] || {{ echo \"✗ webroot 없음: $WEBROOT (Rhymix 설치 먼저)\"; rm -rf \"$STAGING\"; exit 1; }}\n\
         for K in modules layouts; do\n\
           [ -d \"$STAGING/$K\" ] || continue\n\
           mkdir -p \"$WEBROOT/$K\"\n\
           for d in \"$STAGING/$K\"/*/; do [ -d \"$d\" ] || continue; n=$(basename \"$d\");\n\
             rm -rf \"$WEBROOT/$K/$n\"; cp -a \"$d\" \"$WEBROOT/$K/$n\"; echo \"  + $K/$n\"; done\n\
           chown -R \"$VUSER:$VUSER\" \"$WEBROOT/$K\"\n\
         done\n\
         rm -rf \"$STAGING\"\n\
         rm -rf \"$WEBROOT/files/cache\"/* 2>/dev/null || true\n\
         echo \"== 업로드 완료 (Rhymix 캐시 비움) ==\"\n",
        vu = sq(vuser), d = sq(&domain), st = sq(&staging),
    );
    let (sudo_script, sshpass, env) = eondcms_exec(server, &remote_raw, use_root, c.sudo);
    let script = format!("{rs}{sudo_script}");
    Ok(Job {
        title: format!("Rhymix 모듈/레이아웃 업로드 : {domain_name}"),
        script,
        sshpass,
        env,
        note: format!("로컬 {src} → {user}@{host}:{webroot}  (modules: {}, layouts: {})",
            if mods.is_empty() { "-".into() } else { mods.join(",") },
            if lays.is_empty() { "-".into() } else { lays.join(",") }),
    })
}

/// 비-git 설치본을 위한 오버레이/얕은 업데이트 공용 셸 (소유자 권한에서 실행). $WR/$REPO/$EXCL 사용. 작은따옴표 금지.
const CMS_UPDATE_INNER: &str = r#"set -e
if [ -d "$WR/.git" ]; then
  cd "$WR"
  BR="$(git rev-parse --abbrev-ref HEAD 2>/dev/null)"; [ -z "$BR" -o "$BR" = HEAD ] && BR=master
  echo "[git 설치본] depth=1 fetch ($BR)"
  git fetch --depth=1 origin "$BR"
  git reset --hard FETCH_HEAD
  git reflog expire --expire=now --all 2>/dev/null || true
  git gc --prune=now --quiet 2>/dev/null || true
  echo "현재 커밋: $(git rev-parse --short HEAD 2>/dev/null)"
else
  echo "[일반 설치본] 최신본 오버레이 업데이트 (보존: $EXCL)"
  command -v rsync >/dev/null || { echo "rsync 없음 — 오버레이 불가"; exit 1; }
  TMP="$(mktemp -d)"
  git clone --depth 1 "$REPO" "$TMP"
  rsync -a --info=stats1 $EXCL "$TMP"/ "$WR"/
  rm -rf "$TMP"
  echo "오버레이 완료(다음부터 git 관리): $(cd "$WR" && git rev-parse --short HEAD 2>/dev/null)"
fi
echo ".git 용량: $(du -sh "$WR/.git" 2>/dev/null | cut -f1)"
echo "▶ DB 스키마 변경이 있으면 브라우저 관리자에서 마무리될 수 있습니다."
"#;

/// Rhymix/그누보드 업데이트 — git 설치본은 얕은 업데이트, 일반(비-git) 설치본은 최신본 오버레이(사용자 데이터 보존, 이후 git 전환).
/// use_root=true: 설정 SSH(sudo 유저, 예 tong) → sudo → root → `sudo -u <소유자>` 로 실행 (vhost 비번 불필요).
/// use_root=false: vhost(=FTP) 유저로 직접 실행 (FTP 비번 필요, root 불필요).
fn build_git_cms_update(server: &Site, c: &CmsInstall, s: &Settings, domain_name: &str, use_root: bool, g: &GitCms) -> Result<Job, String> {
    let domain = to_ascii_domain(domain_name);
    let owner = c.hestia_user.trim().to_string();
    if owner.is_empty() {
        return Err("HestiaCP 유저(vhost 계정)가 비어 있습니다 — 사이트 불러오기 또는 정보 탭으로 채우세요".into());
    }
    let excl = g.preserve.iter().map(|p| format!("--exclude=/{p}/")).collect::<Vec<_>>().join(" ");
    if use_root {
        // 설정 SSH(tong) → sudo → root, 그 후 sudo -u <소유자> 로 업데이트 → vhost FTP 비번 불필요
        let host = if s.ssh_host.trim().is_empty() { s.hestia_host.trim() } else { s.ssh_host.trim() };
        if host.is_empty() { return Err("서버 SSH 호스트가 비어 있습니다 (설정 > 서버 SSH)".into()); }
        let suser = s.ssh_user.trim();
        if suser.is_empty() { return Err("SSH 유저(sudo 권한, 예: tong)가 비어 있습니다 (설정 > 서버 SSH)".into()); }
        if s.ssh_pass.is_empty() { return Err("SSH 비밀번호가 비어 있습니다 (설정 > 서버 SSH)".into()); }
        let srv = Site {
            ip: host.to_string(),
            ftp_id: suser.to_string(),
            ftp_pw: s.ssh_pass.clone(),
            ssh_port: s.ssh_port.trim().to_string(),
            ..Default::default()
        };
        // INNER 는 작은따옴표가 없으므로 '<INNER>' 로 감싸 sudo -u 의 bash -c 로 소유자 권한 실행
        let raw = format!(
            "set -e\nexport PATH=\"$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin\"\n\
             OWN={own}\nDOMAIN={dom}\nCMSNAME={cn}\nREPO={repo}\nEXCL={excl}\n\
             WR=\"/home/$OWN/web/$DOMAIN/public_html\"\n\
             echo \"== [$CMSNAME 업데이트] $DOMAIN (root/sudo · 소유 $OWN) ==\"\n\
             [ -d \"$WR\" ] || {{ echo \"✗ 웹루트 없음: $WR\"; exit 1; }}\n\
             INNER='{inner}'\n\
             sudo -u \"$OWN\" WR=\"$WR\" REPO=\"$REPO\" EXCL=\"$EXCL\" bash -c \"$INNER\"\n\
             chown -R \"$OWN:$OWN\" \"$WR\" && echo \"  ✓ 소유권 보정: $WR → $OWN:$OWN\"\n\
             echo \"== $CMSNAME 업데이트 완료: $DOMAIN ==\"\n",
            own = sq(&owner), dom = sq(&domain), cn = sq(g.name), repo = sq(g.repo), excl = sq(&excl), inner = CMS_UPDATE_INNER,
        );
        let (script, sshpass, env) = eondcms_exec(&srv, &raw, false, true);
        return Ok(Job {
            title: format!("{} 업데이트 : {domain_name} (root/sudo, 소유 {owner})", g.name),
            script,
            sshpass,
            env,
            note: format!("{suser}@{host} → sudo 로 root 후 sudo -u {owner} 로 업데이트 (git/일반설치 자동)"),
        });
    }
    // root 미선택: vhost(=FTP) 유저 직접 접속 (FTP 비번 필요)
    if server.ip.trim().is_empty() { return Err("대상 서버 IP가 비어 있습니다 (정보 탭에서 입력)".into()); }
    let vuser = server.login_id(false).trim().to_string();
    if vuser.is_empty() { return Err("FTP 계정(=HestiaCP 유저)이 비어 있습니다 (정보 탭에서 입력)".into()); }
    if server.login_pw(false).is_empty() {
        return Err("FTP 비밀번호가 비어 있습니다 (정보 탭에서 입력) — 또는 '루트로 실행'을 켜면 설정의 SSH(tong) 계정으로 업데이트합니다".into());
    }
    let raw = format!(
        "set -e\nexport PATH=\"$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin\"\n\
         VUSER={u}\nDOMAIN={d}\nCMSNAME={cn}\nREPO={repo}\nEXCL={excl}\n\
         WR=\"$HOME/web/$DOMAIN/public_html\"\n[ -d \"$WR\" ] || WR=\"/home/$VUSER/web/$DOMAIN/public_html\"\n\
         echo \"== [$CMSNAME 업데이트] $DOMAIN (vhost 유저 $VUSER · root 불필요) ==\"\n\
         [ -d \"$WR\" ] || {{ echo \"✗ 웹루트 없음: $WR\"; exit 1; }}\n\
         {inner}\n\
         echo \"== $CMSNAME 업데이트 완료: $DOMAIN ==\"\n",
        u = sq(&vuser), d = sq(&domain), cn = sq(g.name), repo = sq(g.repo), excl = sq(&excl), inner = CMS_UPDATE_INNER,
    );
    let (script, sshpass, env) = eondcms_exec(server, &raw, false, false);
    Ok(Job {
        title: format!("{} 업데이트 : {domain_name} (root 불필요)", g.name),
        script,
        sshpass,
        env,
        note: "vhost(FTP) 유저로 직접 업데이트 (git/일반설치 자동)".into(),
    })
}

/// 서버 전체 일괄 업데이트: sudo 유저(예: tong)로 SSH → sudo 로 root → /home/*/web/*/public_html 순회.
/// .git 있으면 소유 유저로 얕은 업데이트(depth=1), 없으면 "선택 필요"로 보고만.
pub fn build_bulk_git_update(s: &Settings) -> Result<Job, String> {
    let host = if s.ssh_host.trim().is_empty() { s.hestia_host.trim() } else { s.ssh_host.trim() };
    if host.is_empty() { return Err("서버 SSH 호스트가 비어 있습니다 (설정)".into()); }
    let user = s.ssh_user.trim();
    if user.is_empty() { return Err("SSH 유저(sudo 권한, 예: tong)가 비어 있습니다 (설정)".into()); }
    if s.ssh_pass.is_empty() { return Err("SSH 비밀번호가 비어 있습니다 (설정)".into()); }
    // sudo 경유 SSH 재사용을 위해 합성 Site 구성 (FTP 계정칸 = sudo 유저)
    let srv = Site {
        ip: host.to_string(),
        ftp_id: user.to_string(),
        ftp_pw: s.ssh_pass.clone(),
        ssh_port: s.ssh_port.trim().to_string(),
        ..Default::default()
    };
    // 단일 인용부호 heredoc 안에서 원격 bash 로 실행됨 (로컬 확장 없음)
    let raw = r#"set -o pipefail
export PATH="$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin"
echo "== Rhymix/그누보드 일괄 업데이트 (서버 전체 git 사이트) =="
shopt -s nullglob
OK=0; SKIP=0; FAIL=0
for WR in /home/*/web/*/public_html; do
  [ -d "$WR" ] || continue
  DOM="$(basename "$(dirname "$WR")")"
  OWN="$(stat -c %U "$WR" 2>/dev/null)"
  if [ -d "$WR/.git" ]; then
    echo "── [$DOM] git 얕은 업데이트 (유저 $OWN) ──"
    BR="$(sudo -u "$OWN" git -C "$WR" rev-parse --abbrev-ref HEAD 2>/dev/null)"; [ -z "$BR" -o "$BR" = HEAD ] && BR=master
    if sudo -u "$OWN" git -C "$WR" fetch --depth=1 origin "$BR" && sudo -u "$OWN" git -C "$WR" reset --hard FETCH_HEAD; then
      sudo -u "$OWN" git -C "$WR" reflog expire --expire=now --all 2>/dev/null || true
      sudo -u "$OWN" git -C "$WR" gc --prune=now --quiet 2>/dev/null || true
      echo "   ✓ $(sudo -u "$OWN" git -C "$WR" rev-parse --short HEAD 2>/dev/null)  (.git $(du -sh "$WR/.git" 2>/dev/null | awk '{print $1}'))"
      OK=$((OK+1))
    else
      echo "   ✗ 실패: $DOM"; FAIL=$((FAIL+1))
    fi
  else
    echo "── [$DOM] git 아님 → 수동/선택 필요: $WR"; SKIP=$((SKIP+1))
  fi
done
echo "== 완료: 업데이트 $OK · git아님(선택필요) $SKIP · 실패 $FAIL =="
"#;
    let (script, sshpass, env) = eondcms_exec(&srv, raw, false, true);
    Ok(Job {
        title: "Rhymix/그누보드 일괄 업데이트 (서버 git 사이트)".into(),
        script,
        sshpass,
        env,
        note: format!("{user}@{host} → sudo 로 root 후 /home/*/web/*/public_html 순회 (git=얕은 업데이트, 비-git=보고)"),
    })
}

/// 입력이 영숫자/._- 로만 이뤄졌는지 (경로탈출/쉘주입 방지)
fn is_safe_name(s: &str) -> bool {
    !s.is_empty() && !s.contains("..") && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

/// Rhymix 모듈 일괄/계정별 삭제: sudo 유저로 root 후 webroot/modules/<모듈> 제거.
/// account 가 비면 서버 전체(/home/*), 있으면 해당 계정(/home/<account>)만.
pub fn build_module_delete(s: &Settings, module: &str, account: &str) -> Result<Job, String> {
    let host = if s.ssh_host.trim().is_empty() { s.hestia_host.trim() } else { s.ssh_host.trim() };
    if host.is_empty() { return Err("서버 SSH 호스트가 비어 있습니다 (설정)".into()); }
    let user = s.ssh_user.trim();
    if user.is_empty() { return Err("SSH 유저(sudo 권한, 예: tong)가 비어 있습니다 (설정)".into()); }
    if s.ssh_pass.is_empty() { return Err("SSH 비밀번호가 비어 있습니다 (설정)".into()); }
    let m = module.trim();
    if !is_safe_name(m) { return Err("모듈 이름은 영숫자/._- 만 허용됩니다".into()); }
    let acct = account.trim();
    if !acct.is_empty() && !is_safe_name(acct) { return Err("계정 이름은 영숫자/._- 만 허용됩니다".into()); }
    let home = if acct.is_empty() { "/home/*".to_string() } else { format!("/home/{acct}") };
    let scope = if acct.is_empty() { "서버 전체".to_string() } else { format!("계정 {acct}") };
    let srv = Site {
        ip: host.to_string(),
        ftp_id: user.to_string(),
        ftp_pw: s.ssh_pass.clone(),
        ssh_port: s.ssh_port.trim().to_string(),
        ..Default::default()
    };
    // home 글롭(/home/* 또는 /home/acct)은 원격에서 확장되도록 비인용 삽입. acct 는 위에서 검증됨.
    let raw = format!(
        "set -o pipefail\n\
         export PATH=\"$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin\"\n\
         MOD={mod}\n\
         echo \"== Rhymix 모듈 삭제: '$MOD' (대상 {scope}) ==\"\n\
         shopt -s nullglob\n\
         CNT=0\n\
         for WR in {home}/web/*/public_html; do\n\
           [ -d \"$WR\" ] || continue\n\
           DOM=\"$(basename \"$(dirname \"$WR\")\")\"\n\
           TARGET=\"$WR/modules/$MOD\"\n\
           if [ -d \"$TARGET\" ]; then\n\
             OWN=\"$(stat -c %U \"$WR\" 2>/dev/null)\"\n\
             if rm -rf \"$TARGET\"; then\n\
               echo \"  ✓ 삭제: [$DOM] $TARGET (소유 $OWN)\"\n\
               rm -rf \"$WR/files/cache/\"* 2>/dev/null || true\n\
               CNT=$((CNT+1))\n\
             else\n\
               echo \"  ✗ 실패: [$DOM] $TARGET\"\n\
             fi\n\
           fi\n\
         done\n\
         echo \"== 완료: $CNT 곳에서 '$MOD' 모듈 삭제 (캐시 정리 포함) ==\"\n",
        mod = sq(m), scope = scope, home = home,
    );
    let (script, sshpass, env) = eondcms_exec(&srv, &raw, false, true);
    Ok(Job {
        title: format!("Rhymix 모듈 삭제 '{m}' ({scope})"),
        script,
        sshpass,
        env,
        note: format!("{user}@{host} → sudo 로 root 후 {home}/web/*/public_html/modules/{m} 제거"),
    })
}

/// 설정 SSH 자격증명으로 합성 Site(=tong 등) 구성. 일괄/계정 작업 공용.
fn ssh_admin_site(s: &Settings) -> Result<Site, String> {
    let host = if s.ssh_host.trim().is_empty() { s.hestia_host.trim() } else { s.ssh_host.trim() };
    if host.is_empty() { return Err("서버 SSH 호스트가 비어 있습니다 (설정)".into()); }
    let user = s.ssh_user.trim();
    if user.is_empty() { return Err("SSH 유저(sudo 권한, 예: tong)가 비어 있습니다 (설정)".into()); }
    if s.ssh_pass.is_empty() { return Err("SSH 비밀번호가 비어 있습니다 (설정)".into()); }
    Ok(Site {
        ip: host.to_string(),
        ftp_id: user.to_string(),
        ftp_pw: s.ssh_pass.clone(),
        ssh_port: s.ssh_port.trim().to_string(),
        ..Default::default()
    })
}

/// 모듈 목록 조회 → (모듈명, 사용 도메인들) 정렬. domain=Some 이면 그 도메인만, None 이면 계정 전체.
pub fn list_account_modules(s: &Settings, account: &str, domain: Option<&str>) -> Result<Vec<(String, Vec<String>)>, String> {
    let acct = account.trim();
    if !is_safe_name(acct) { return Err("계정 이름 형식 오류 (영숫자/._- 만)".into()); }
    let webglob = match domain {
        Some(d) => {
            let da = to_ascii_domain(d);
            if !is_safe_name(&da) { return Err(format!("도메인 형식 오류: {d}")); }
            format!("/home/{acct}/web/{da}/public_html")
        }
        None => format!("/home/{acct}/web/*/public_html"),
    };
    let srv = ssh_admin_site(s)?;
    let raw = format!(
        "shopt -s nullglob\n\
         for D in {webglob}/modules/*/; do\n\
           [ -d \"$D\" ] || continue\n\
           M=\"$(basename \"$D\")\"\n\
           DOM=\"$(basename \"$(dirname \"$(dirname \"$D\")\")\")\"\n\
           echo \"$M|$DOM\"\n\
         done\n",
        webglob = webglob,
    );
    let (script, sshpass, mut env) = eondcms_exec(&srv, &raw, false, true);
    env.push(("SSHPASS".to_string(), sshpass));
    let out = run_capture(&script, &env)?;
    let mut map: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() { continue; }
        if let Some((m, dom)) = line.split_once('|') {
            let e = map.entry(m.to_string()).or_default();
            if !e.contains(&dom.to_string()) { e.push(dom.to_string()); }
        }
    }
    Ok(map.into_iter().collect())
}

/// 선택 모듈 삭제 (modules/<이름> 제거 + 캐시 정리). domain=Some 이면 그 도메인만, None 이면 계정 전체.
pub fn build_account_modules_delete(s: &Settings, account: &str, domain: Option<&str>, modules: &[String]) -> Result<Job, String> {
    let acct = account.trim();
    if !is_safe_name(acct) { return Err("계정 이름 형식 오류 (영숫자/._- 만)".into()); }
    if modules.is_empty() { return Err("삭제할 모듈을 선택하세요".into()); }
    for m in modules {
        if !is_safe_name(m.trim()) { return Err(format!("모듈 이름 형식 오류: {m}")); }
    }
    let (webglob, scope) = match domain {
        Some(d) => {
            let da = to_ascii_domain(d);
            if !is_safe_name(&da) { return Err(format!("도메인 형식 오류: {d}")); }
            (format!("/home/{acct}/web/{da}/public_html"), format!("도메인 {da}"))
        }
        None => (format!("/home/{acct}/web/*/public_html"), format!("계정 {acct} 전체")),
    };
    let srv = ssh_admin_site(s)?;
    let list: Vec<String> = modules.iter().map(|m| m.trim().to_string()).collect();
    // 공백 구분 MODS (모듈명은 위에서 검증되어 공백/특수문자 없음)
    let mods = list.join(" ");
    let raw = format!(
        "set -o pipefail\n\
         export PATH=\"$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin\"\n\
         MODS={mods}\n\
         echo \"== 모듈 삭제 ({scope}): $MODS ==\"\n\
         shopt -s nullglob\n\
         CNT=0\n\
         for WR in {webglob}; do\n\
           [ -d \"$WR\" ] || continue\n\
           DOM=\"$(basename \"$(dirname \"$WR\")\")\"\n\
           CACHED=0\n\
           for M in $MODS; do\n\
             T=\"$WR/modules/$M\"\n\
             if [ -d \"$T\" ]; then\n\
               if rm -rf \"$T\"; then echo \"  ✓ [$DOM] $M 삭제\"; CNT=$((CNT+1)); CACHED=1; else echo \"  ✗ [$DOM] $M 실패\"; fi\n\
             fi\n\
           done\n\
           [ \"$CACHED\" = 1 ] && rm -rf \"$WR/files/cache/\"* 2>/dev/null || true\n\
         done\n\
         echo \"== 완료: $CNT 곳에서 모듈 삭제 (캐시 정리 포함) ==\"\n",
        mods = sq(&mods), scope = scope, webglob = webglob,
    );
    let (script, sshpass, env) = eondcms_exec(&srv, &raw, false, true);
    Ok(Job {
        title: format!("모듈 삭제 — {scope} ({}개)", list.len()),
        script,
        sshpass,
        env,
        note: format!("{webglob}/modules/{{{}}} 제거", list.join(",")),
    })
}

/// CMS 감지 + 버전/DB/용량 산출 ($WR 기준). 결과: KIND/VER/GIT/DB/FB/DBB. eondcms 는 형제 pythonapp/ 로 판별.
const CMS_DETECT: &str = r#"  KIND=unknown; VER="-"; DB=""; SIZEDIR="$WR"; WEBDIR="$(dirname "$WR")"
  if [ -d "$WEBDIR/pythonapp" ] && { [ -f "$WEBDIR/pythonapp/app/main.py" ] || [ -d "$WEBDIR/pythonapp/.venv" ] || [ -f "$WEBDIR/pythonapp/pyproject.toml" ]; }; then
    KIND=eondcms; SIZEDIR="$WEBDIR"
    VER="$(grep -oP '^version\s*=\s*"\K[^"]+' "$WEBDIR/pythonapp/pyproject.toml" 2>/dev/null | head -1)"
    DB="$(grep -oP 'DATABASE_URL=.*/\K[^?[:space:]]+' "$WEBDIR/pythonapp/.env" 2>/dev/null | head -1)"
    [ -z "$DB" ] && DB="$(grep -oP '^DB_NAME=\K.*' "$WEBDIR/pythonapp/.env" 2>/dev/null | head -1)"
  elif [ -f "$WR/wp-load.php" ]; then
    KIND=WordPress
    VER="$(awk -F"'" '/\$wp_version =/{print $2; exit}' "$WR/wp-includes/version.php" 2>/dev/null)"
    DB="$(grep -oP "DB_NAME'?\s*,\s*'\K[^']+" "$WR/wp-config.php" 2>/dev/null | head -1)"
  elif [ -f "$WR/common/constants.php" ] || [ -d "$WR/common/framework" ]; then
    KIND=Rhymix
    VER="$(grep -oP "RX_VERSION'\s*,\s*'\K[^']+" "$WR/common/constants.php" 2>/dev/null | head -1)"
    for CF in "$WR/files/config/db.config.php" "$WR/config/db.config.php"; do
      [ -f "$CF" ] || continue
      DB="$(grep -oP "'database'\s*=>\s*'\K[^']+" "$CF" 2>/dev/null | head -1)"
      [ -z "$DB" ] && DB="$(grep -oP "db_database'?\s*[=,]\s*[\"']\K[^\"']+" "$CF" 2>/dev/null | head -1)"
      [ -n "$DB" ] && break
    done
  elif [ -f "$WR/config/config.inc.php" ] && [ -d "$WR/classes" ]; then
    KIND=XE
    DB="$(grep -oP "db_database'?\s*[=,]\s*[\"']\K[^\"']+" "$WR/files/config/db.config.php" "$WR/config/db.config.php" 2>/dev/null | head -1)"
  elif { [ -f "$WR/common.php" ] && [ -d "$WR/bbs" ]; } || [ -f "$WR/bbs/login.php" ]; then
    KIND=Gnuboard
    VER="$(grep -hoP "G5_GNUBOARD_VER'\s*,\s*'\K[0-9][^']*" "$WR/version.php" "$WR/config.php" 2>/dev/null | head -1)"
    [ -z "$VER" ] && VER="-"
    [ -f "$WR/data/dbconfig.php" ] && DB="$(grep -oP "mysql_db'?\s*[,=]\s*[\"']\K[^\"']+" "$WR/data/dbconfig.php" 2>/dev/null | head -1)"
  fi
  GIT=no
  if [ -d "$WR/.git" ]; then GIT=yes; if [ -z "$VER" ] || [ "$VER" = "-" ]; then VER="$(git -C "$WR" describe --tags --always 2>/dev/null)"; fi; fi
  [ -z "$VER" ] && VER="-"
  FB="$(du -sb "$SIZEDIR" 2>/dev/null | cut -f1)"; [ -z "$FB" ] && FB=0
  DBB=0
  if [ -n "$DB" ]; then DBB="$(mysql -N -B -e "SELECT IFNULL(SUM(data_length+index_length),0) FROM information_schema.tables WHERE table_schema='$DB'" 2>/dev/null)"; [ -z "$DBB" ] && DBB=0; fi
"#;

/// 최신 버전 조회(스크립트 1회 실행) — Rhymix/WordPress 공개 최신 버전.
const CMS_LATEST_PRELUDE: &str = r#"LATEST_RX="$(curl -fsSL https://raw.githubusercontent.com/rhymix/rhymix/master/common/constants.php 2>/dev/null | grep -oP "RX_VERSION'\s*,\s*'\K[^']+" | head -1)"
LATEST_WP="$(curl -fsSL https://api.wordpress.org/core/version-check/ 2>/dev/null | grep -oP '"current":"\K[^"]+' | head -1)"
"#;

/// 설치 버전 vs 최신 버전 → STATUS ("최신" / "업데이트→x.y" / "확인필요" / "-").
const CMS_STATUS: &str = r#"  STATUS="-"
  case "$VER" in
    *.*)
      if [ "$KIND" = Rhymix ] && [ -n "$LATEST_RX" ]; then if [ "$VER" = "$LATEST_RX" ]; then STATUS="최신버전"; else STATUS="업데이트필요 ($LATEST_RX)"; fi; fi
      if [ "$KIND" = WordPress ] && [ -n "$LATEST_WP" ]; then if [ "$VER" = "$LATEST_WP" ]; then STATUS="최신버전"; else STATUS="업데이트필요 ($LATEST_WP)"; fi; fi
      ;;
    *) [ "$GIT" = yes ] && STATUS="확인필요";;
  esac
"#;

/// 계정의 사이트별 CMS 종류·버전·상태·git·파일/DB 용량 스캔.
pub fn scan_account_sites(s: &Settings, account: &str) -> Result<Vec<(String, String, String, String, bool, u64, u64)>, String> {
    let acct = account.trim();
    if !is_safe_name(acct) { return Err("계정 이름 형식 오류 (영숫자/._- 만)".into()); }
    let srv = ssh_admin_site(s)?;
    let body = format!(
        "shopt -s nullglob\n{prelude}for WR in /home/$OWN/web/*/public_html; do\n  [ -d \"$WR\" ] || continue\n  DOM=\"$(basename \"$(dirname \"$WR\")\")\"\n{detect}{status}  echo \"$DOM|$KIND|$VER|$STATUS|$GIT|$FB|$DBB\"\ndone\n",
        prelude = CMS_LATEST_PRELUDE, detect = CMS_DETECT, status = CMS_STATUS,
    );
    let raw = format!("export PATH=\"$PATH:/usr/local/bin:/usr/bin:/bin\"\nOWN={}\n{}", sq(acct), body);
    let (script, sshpass, mut env) = eondcms_exec(&srv, &raw, false, true);
    env.push(("SSHPASS".to_string(), sshpass));
    let out = run_capture(&script, &env)?;
    let mut res = Vec::new();
    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() { continue; }
        let f: Vec<&str> = line.splitn(7, '|').collect();
        if f.len() == 7 {
            let fb = f[5].trim().parse::<u64>().unwrap_or(0);
            let dbb = f[6].trim().parse::<u64>().unwrap_or(0);
            res.push((f[0].to_string(), f[1].to_string(), f[2].to_string(), f[3].to_string(), f[4] == "yes", fb, dbb));
        }
    }
    Ok(res)
}

/// 한 사이트 업데이트 스텝(소유자 $OWN, 웹루트 $WR 기준). CMS 유형 자동 판별:
/// WordPress=wp-cli, git 설치본=얕은 업데이트, 일반(비-git) Rhymix/그누보드=최신본 오버레이.
const SITE_UPDATE_STEP: &str = r#"  if [ -f "$WR/wp-load.php" ]; then
    WP=/usr/local/bin/wp
    if ! [ -x "$WP" ]; then echo "wp-cli 설치"; curl -fsSL https://raw.githubusercontent.com/wp-cli/builds/gh-pages/phar/wp-cli.phar -o "$WP" 2>/dev/null && chmod +x "$WP"; fi
    echo "── [$OWN/$DOM] WordPress 업데이트 (wp-cli) ──"
    if sudo -u "$OWN" "$WP" --path="$WR" core update; then
      sudo -u "$OWN" "$WP" --path="$WR" core update-db || true
      sudo -u "$OWN" "$WP" --path="$WR" plugin update --all || true
      sudo -u "$OWN" "$WP" --path="$WR" theme update --all || true
      sudo -u "$OWN" "$WP" --path="$WR" language core update || true
      echo "   OK $(sudo -u "$OWN" "$WP" --path="$WR" core version 2>/dev/null)"; OK=$((OK+1))
    else echo "   FAIL"; FAIL=$((FAIL+1)); fi
  elif [ -d "$WR/.git" ]; then
    BR="$(sudo -u "$OWN" git -C "$WR" rev-parse --abbrev-ref HEAD 2>/dev/null)"; [ -z "$BR" -o "$BR" = HEAD ] && BR=master
    echo "── [$OWN/$DOM] git 얕은 업데이트 ($BR) ──"
    if sudo -u "$OWN" git -C "$WR" fetch --depth=1 origin "$BR" && sudo -u "$OWN" git -C "$WR" reset --hard FETCH_HEAD; then
      sudo -u "$OWN" git -C "$WR" reflog expire --expire=now --all 2>/dev/null || true
      sudo -u "$OWN" git -C "$WR" gc --prune=now --quiet 2>/dev/null || true
      echo "   OK $(sudo -u "$OWN" git -C "$WR" rev-parse --short HEAD 2>/dev/null)"; OK=$((OK+1))
    else echo "   FAIL"; FAIL=$((FAIL+1)); fi
  else
    REPO=""; EXCL=""
    if [ -d "$WR/common/framework" ] || [ -f "$WR/common/constants.php" ] || [ -f "$WR/config/config.inc.php" ]; then REPO="https://github.com/rhymix/rhymix.git"; EXCL="--exclude=/config/ --exclude=/files/"
    elif [ -f "$WR/common.php" ] && [ -d "$WR/bbs" ]; then REPO="https://github.com/gnuboard/gnuboard5.git"; EXCL="--exclude=/data/"
    fi
    if [ -z "$REPO" ]; then echo "── [$OWN/$DOM] CMS 미상 → 건너뜀"; SKIP=$((SKIP+1)); continue; fi
    echo "── [$OWN/$DOM] 일반설치 오버레이 업데이트 (보존 $EXCL) ──"
    TMP="$(sudo -u "$OWN" mktemp -d)"
    if sudo -u "$OWN" git clone --depth 1 "$REPO" "$TMP" && sudo -u "$OWN" rsync -a $EXCL "$TMP"/ "$WR"/; then
      sudo -u "$OWN" rm -rf "$TMP"; echo "   OK 오버레이 완료 (다음부터 git)"; OK=$((OK+1))
    else sudo -u "$OWN" rm -rf "$TMP" 2>/dev/null; echo "   FAIL"; FAIL=$((FAIL+1)); fi
  fi
"#;

/// 계정 사이트 일괄 업데이트 — domains 비면 전체, 아니면 지정 도메인만.
pub fn build_account_git_update(s: &Settings, account: &str, domains: &[String]) -> Result<Job, String> {
    let acct = account.trim();
    if !is_safe_name(acct) { return Err("계정 이름 형식 오류 (영숫자/._- 만)".into()); }
    let srv = ssh_admin_site(s)?;
    let list = if domains.is_empty() {
        "/home/$OWN/web/*/public_html".to_string()
    } else {
        let mut parts = Vec::new();
        for d in domains {
            let da = to_ascii_domain(d);
            if !is_safe_name(&da) { return Err(format!("도메인 형식 오류: {d}")); }
            parts.push(format!("\"/home/$OWN/web/{da}/public_html\""));
        }
        parts.join(" ")
    };
    let scope = if domains.is_empty() { "전체".to_string() } else { format!("{}개 선택", domains.len()) };
    let body = format!(
        "shopt -s nullglob\nOK=0; SKIP=0; FAIL=0\n\
         for WR in {list}; do\n  [ -d \"$WR\" ] || continue\n  DOM=\"$(basename \"$(dirname \"$WR\")\")\"\n{step}\n  chown -R \"$OWN:$OWN\" \"$WR\" 2>/dev/null || true\ndone\n\
         echo \"== 완료: 업데이트 $OK · 건너뜀 $SKIP · 실패 $FAIL (소유권 $OWN 로 보정) ==\"\n",
        list = list, step = SITE_UPDATE_STEP,
    );
    let raw = format!("export PATH=\"$PATH:/usr/local/bin:/usr/bin:/bin\"\nOWN={}\n{}", sq(acct), body);
    let (script, sshpass, env) = eondcms_exec(&srv, &raw, false, true);
    Ok(Job {
        title: format!("계정 {acct} 사이트 업데이트 ({scope})"),
        script,
        sshpass,
        env,
        note: format!("/home/{acct}/web/*/public_html 업데이트 (git=얕은, 일반=오버레이; sudo -u {acct})"),
    })
}

/// 전체 사이트 스캔 — 모든 계정 /home/*/web/*/public_html → (계정, 도메인, 종류, 버전, git, 파일바이트, DB바이트).
pub fn scan_all_sites(s: &Settings) -> Result<Vec<(String, String, String, String, String, bool, u64, u64)>, String> {
    let srv = ssh_admin_site(s)?;
    let body = format!(
        "shopt -s nullglob\n{prelude}for WR in /home/*/web/*/public_html; do\n  [ -d \"$WR\" ] || continue\n  OWN=\"$(echo \"$WR\" | awk -F/ '{{print $3}}')\"\n  DOM=\"$(basename \"$(dirname \"$WR\")\")\"\n{detect}{status}  echo \"$OWN|$DOM|$KIND|$VER|$STATUS|$GIT|$FB|$DBB\"\ndone\n",
        prelude = CMS_LATEST_PRELUDE, detect = CMS_DETECT, status = CMS_STATUS,
    );
    let raw = format!("export PATH=\"$PATH:/usr/local/bin:/usr/bin:/bin\"\n{}", body);
    let (script, sshpass, mut env) = eondcms_exec(&srv, &raw, false, true);
    env.push(("SSHPASS".to_string(), sshpass));
    let out = run_capture(&script, &env)?;
    let mut res = Vec::new();
    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() { continue; }
        let f: Vec<&str> = line.splitn(8, '|').collect();
        if f.len() == 8 {
            let fb = f[6].trim().parse::<u64>().unwrap_or(0);
            let dbb = f[7].trim().parse::<u64>().unwrap_or(0);
            res.push((f[0].to_string(), f[1].to_string(), f[2].to_string(), f[3].to_string(), f[4].to_string(), f[5] == "yes", fb, dbb));
        }
    }
    Ok(res)
}

/// 로컬에서 Rhymix/WordPress/그누보드 최신 '정식 릴리스' 버전 조회 → (rhymix, wordpress, gnuboard). 실패 시 빈 문자열.
pub fn latest_versions() -> (String, String, String) {
    fn http(url: &str) -> Option<String> {
        // GitHub API 는 User-Agent 필수
        let o = std::process::Command::new("curl").args(["-fsSL", "--max-time", "8", "-A", "hostmover", url]).output().ok()?;
        if o.status.success() { Some(String::from_utf8_lossy(&o.stdout).to_string()) } else { None }
    }
    fn between<'a>(s: &'a str, start: &str, end: &str) -> Option<&'a str> {
        let i = s.find(start)? + start.len();
        let rest = &s[i..];
        let j = rest.find(end)?;
        Some(&rest[..j])
    }
    // Rhymix 최신 정식 태그 = git ls-remote 의 최대 버전 태그 (클론 되는 환경이면 항상 동작).
    let rx = latest_git_tag("https://github.com/rhymix/rhymix.git");
    let mut wp = http("https://api.wordpress.org/core/version-check/")
        .and_then(|t| between(&t, "\"current\":\"", "\"").map(|v| v.to_string()))
        .unwrap_or_default();
    if wp.is_empty() { wp = latest_git_tag("https://github.com/WordPress/WordPress.git"); }
    let gn = latest_git_tag("https://github.com/gnuboard/gnuboard5.git");
    (rx, wp.trim().to_string(), gn)
}

/// 원격 git 저장소의 최신(최대) 버전 태그. 숫자 컴포넌트로 비교(문자열 정렬 오류 방지). 실패 시 빈 문자열.
fn latest_git_tag(repo: &str) -> String {
    let o = match std::process::Command::new("git").args(["ls-remote", "--tags", "--refs", "--", repo]).output() {
        Ok(o) if o.status.success() => o,
        _ => return String::new(),
    };
    let text = String::from_utf8_lossy(&o.stdout);
    let key = |t: &str| -> Vec<u64> {
        t.trim_start_matches('v').split('.').map(|c| c.chars().take_while(|ch| ch.is_ascii_digit()).collect::<String>().parse::<u64>().unwrap_or(0)).collect()
    };
    let mut best: Option<(Vec<u64>, String)> = None;
    for line in text.lines() {
        let tag = match line.rsplit_once("refs/tags/") { Some((_, t)) => t.trim(), None => continue };
        if !tag.chars().next().map(|c| c.is_ascii_digit() || c == 'v').unwrap_or(false) { continue; }
        let k = key(tag);
        if best.as_ref().map(|(bk, _)| &k > bk).unwrap_or(true) {
            best = Some((k, tag.trim_start_matches('v').to_string()));
        }
    }
    best.map(|(_, v)| v).unwrap_or_default()
}

/// 도메인의 A 레코드(IP)를 로컬에서 조회. dig 우선, 실패 시 시스템 리졸버.
pub fn resolve_a(domain: &str) -> String {
    let d = domain.trim();
    if d.is_empty() { return String::new(); }
    if let Ok(o) = std::process::Command::new("dig").args(["+short", "A", d]).output() {
        let s = String::from_utf8_lossy(&o.stdout);
        let ips: Vec<String> = s.lines().map(|l| l.trim().to_string()).filter(|l| l.parse::<std::net::Ipv4Addr>().is_ok()).collect();
        if !ips.is_empty() { return ips.join(", "); }
    }
    use std::net::ToSocketAddrs;
    if let Ok(addrs) = (d, 80u16).to_socket_addrs() {
        let mut v4: Vec<String> = addrs.filter_map(|a| match a.ip() { std::net::IpAddr::V4(ip) => Some(ip.to_string()), _ => None }).collect();
        v4.sort();
        v4.dedup();
        if !v4.is_empty() { return v4.join(", "); }
    }
    "-".into()
}

/// 선택 사이트의 파일+DB를 로컬로 백업 — 사이트당 tar(웹루트/앱) + mysqldump 를 한 .tar.gz 로 받아 dest 에 저장.
pub fn build_local_backup(s: &Settings, pairs: &[(String, String)], dest: &str) -> Result<Job, String> {
    if pairs.is_empty() { return Err("백업할 사이트를 선택하세요".into()); }
    let srv = ssh_admin_site(s)?;
    let host = srv.ip.clone();
    let user = srv.ftp_id.clone();
    let mut sshpass = String::new();
    let mut blocks = String::new();
    for (a, d) in pairs {
        let aa = a.trim();
        let da = to_ascii_domain(d);
        if !is_safe_name(aa) { return Err(format!("계정 형식 오류: {a}")); }
        if !is_safe_name(&da) { return Err(format!("도메인 형식 오류: {d}")); }
        // 원격: 웹루트(또는 eondcms pythonapp) + DB덤프를 하나의 tar.gz 스트림으로
        let remote = format!(
            "WR=\"/home/{aa}/web/{da}/public_html\"\n\
             WEBDIR=\"$(dirname \"$WR\")\"\n\
             SRC=\"$WR\"; [ -d \"$WEBDIR/pythonapp\" ] && SRC=\"$WEBDIR\"\n\
             DB=\"\"\n\
             if [ -f \"$WEBDIR/pythonapp/.env\" ]; then DB=\"$(grep -oP 'DATABASE_URL=.*/\\K[^?[:space:]]+' \"$WEBDIR/pythonapp/.env\" 2>/dev/null | head -1)\"; fi\n\
             [ -z \"$DB\" ] && [ -f \"$WR/wp-config.php\" ] && DB=\"$(grep -oP \"DB_NAME'?\\s*,\\s*'\\K[^']+\" \"$WR/wp-config.php\" 2>/dev/null | head -1)\"\n\
             [ -z \"$DB\" ] && [ -f \"$WR/files/config/db.config.php\" ] && DB=\"$(grep -oP \"'database'\\s*=>\\s*'\\K[^']+\" \"$WR/files/config/db.config.php\" 2>/dev/null | head -1)\"\n\
             [ -z \"$DB\" ] && [ -f \"$WR/data/dbconfig.php\" ] && DB=\"$(grep -oP \"mysql_db'?\\s*[,=]\\s*[\\\"']\\K[^\\\"']+\" \"$WR/data/dbconfig.php\" 2>/dev/null | head -1)\"\n\
             TMP=\"$(mktemp -d)\"\n\
             [ -n \"$DB\" ] && mysqldump --single-transaction --no-tablespaces \"$DB\" > \"$TMP/db.sql\" 2>/dev/null\n\
             if [ -s \"$TMP/db.sql\" ]; then tar -czf - -C \"$SRC\" . -C \"$TMP\" db.sql; else tar -czf - -C \"$SRC\" .; fi\n\
             rm -rf \"$TMP\"\n",
            aa = aa, da = da,
        );
        let (cmd, pw, _env) = eondcms_exec(&srv, &remote, false, true);
        sshpass = pw;
        blocks += &format!(
            "D={destq}/{accq}\nmkdir -p \"$D\"\nF=\"$D/{da}-$TS.tar.gz\"\necho \"── {aa}/{da} → $F\"\n\
             if {cmd} > \"$F\"; then echo \"  ✓ $(du -h \"$F\" 2>/dev/null | cut -f1)\"; OK=$((OK+1)); else echo \"  ✗ 실패: {aa}/{da}\"; rm -f \"$F\"; FAIL=$((FAIL+1)); fi\n",
            destq = sq(dest), accq = aa, da = da, aa = aa, cmd = cmd,
        );
    }
    let script = format!(
        "TS=$(date +%Y%m%d-%H%M%S)\nOK=0; FAIL=0\necho \"== 선택 사이트 파일·DB 백업 ({n}개) → 로컬 ==\"\n{blocks}echo \"== 완료: 성공 $OK · 실패 $FAIL (저장: {dest}) ==\"\n",
        n = pairs.len(), blocks = blocks, dest = dest,
    );
    Ok(Job {
        title: format!("선택 사이트 파일·DB 백업 ({}개) → 로컬", pairs.len()),
        script,
        sshpass: sshpass.clone(),
        env: vec![("HM_SUDOPW".to_string(), sshpass)],
        note: format!("{user}@{host} → 사이트별 tar(파일)+mysqldump(DB) → {dest}"),
    })
}

/// 선택한 (계정,도메인) 사이트만 스캔 → (계정, 도메인, 종류, 버전, git, 파일, DB). 업데이트 직후 갱신용.
pub fn scan_selected_sites(s: &Settings, pairs: &[(String, String)]) -> Result<Vec<(String, String, String, String, bool, u64, u64)>, String> {
    if pairs.is_empty() { return Ok(Vec::new()); }
    let srv = ssh_admin_site(s)?;
    let mut lines = Vec::new();
    for (a, d) in pairs {
        let aa = a.trim();
        let da = to_ascii_domain(d);
        if !is_safe_name(aa) { return Err(format!("계정 형식 오류: {a}")); }
        if !is_safe_name(&da) { return Err(format!("도메인 형식 오류: {d}")); }
        lines.push(format!("{aa}|{da}"));
    }
    let pairs_text = lines.join("\n");
    let body = format!(
        "while IFS='|' read -r OWN DOM; do\n  [ -n \"$OWN\" ] || continue\n  WR=\"/home/$OWN/web/$DOM/public_html\"\n  [ -d \"$WR\" ] || continue\n{detect}  echo \"$OWN|$DOM|$KIND|$VER|$GIT|$FB|$DBB\"\ndone <<'PAIRS'\n{pairs}\nPAIRS\n",
        detect = CMS_DETECT, pairs = pairs_text,
    );
    let raw = format!("export PATH=\"$PATH:/usr/local/bin:/usr/bin:/bin\"\n{}", body);
    let (script, sshpass, mut env) = eondcms_exec(&srv, &raw, false, true);
    env.push(("SSHPASS".to_string(), sshpass));
    let out = run_capture(&script, &env)?;
    let mut res = Vec::new();
    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() { continue; }
        let f: Vec<&str> = line.splitn(7, '|').collect();
        if f.len() == 7 {
            let fb = f[5].trim().parse::<u64>().unwrap_or(0);
            let dbb = f[6].trim().parse::<u64>().unwrap_or(0);
            res.push((f[0].to_string(), f[1].to_string(), f[2].to_string(), f[3].to_string(), f[4] == "yes", fb, dbb));
        }
    }
    Ok(res)
}

/// 여러 계정의 선택 사이트 일괄 업데이트 — pairs = (계정, 도메인) 목록.
pub fn build_global_update(s: &Settings, pairs: &[(String, String)]) -> Result<Job, String> {
    if pairs.is_empty() { return Err("업데이트할 사이트를 선택하세요".into()); }
    let srv = ssh_admin_site(s)?;
    let mut lines = Vec::new();
    for (a, d) in pairs {
        let aa = a.trim();
        let da = to_ascii_domain(d);
        if !is_safe_name(aa) { return Err(format!("계정 형식 오류: {a}")); }
        if !is_safe_name(&da) { return Err(format!("도메인 형식 오류: {d}")); }
        lines.push(format!("{aa}|{da}"));
    }
    let pairs_text = lines.join("\n");
    let body = format!(
        "shopt -s nullglob\nOK=0; SKIP=0; FAIL=0\n\
         while IFS='|' read -r OWN DOM; do\n  [ -n \"$OWN\" ] || continue\n  WR=\"/home/$OWN/web/$DOM/public_html\"\n  [ -d \"$WR\" ] || {{ echo \"── [$OWN/$DOM] 웹루트 없음\"; SKIP=$((SKIP+1)); continue; }}\n{step}\ndone <<'PAIRS'\n{pairs}\nPAIRS\n\
         echo \"== 완료: 업데이트 $OK · 건너뜀 $SKIP · 실패 $FAIL ==\"\n",
        step = SITE_UPDATE_STEP, pairs = pairs_text,
    );
    let raw = format!("export PATH=\"$PATH:/usr/local/bin:/usr/bin:/bin\"\n{}", body);
    let (script, sshpass, env) = eondcms_exec(&srv, &raw, false, true);
    Ok(Job {
        title: format!("전체 사이트 업데이트 ({}개)", pairs.len()),
        script,
        sshpass,
        env,
        note: "선택한 (계정/도메인) 사이트 업데이트 (git=얕은, 일반=오버레이)".into(),
    })
}

/// WordPress 설치 — v-add-* + wp-cli(core download/config/install) + SSL
fn build_wp_install(server: &Site, c: &CmsInstall, domain_name: &str, use_root: bool) -> Result<Job, String> {
    cms_validate(server, c, use_root, true, true)?;
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
    cms_validate(server, c, use_root, false, false)?;
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

// ===== WordPress 플러그인/테마 동기화 (로컬 dev/wp → 사이트) =====

/// 동기화 대상 종류 (플러그인 vs 테마)
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum WpAssetKind {
    Plugin,
    Theme,
}

impl Default for WpAssetKind {
    fn default() -> Self { WpAssetKind::Plugin }
}

impl WpAssetKind {
    /// wp-content 하위 폴더명 (plugins / themes)
    pub fn subdir(self) -> &'static str {
        match self { WpAssetKind::Plugin => "plugins", WpAssetKind::Theme => "themes" }
    }
    /// wp-cli 서브커맨드 (plugin / theme)
    pub fn wp_cli(self) -> &'static str {
        match self { WpAssetKind::Plugin => "plugin", WpAssetKind::Theme => "theme" }
    }
    /// 한글 라벨
    pub fn label(self) -> &'static str {
        match self { WpAssetKind::Plugin => "플러그인", WpAssetKind::Theme => "테마" }
    }
    /// 헤더에서 이름을 찾을 필드명 (Plugin Name / Theme Name)
    fn name_field(self) -> &'static str {
        match self { WpAssetKind::Plugin => "Plugin Name", WpAssetKind::Theme => "Theme Name" }
    }
}

/// 로컬↔원격 버전 비교 결과
#[derive(Clone, Copy, PartialEq)]
pub enum WpDiff {
    Update,      // 로컬이 더 최신 → 업로드로 업데이트 가능
    Same,        // 동일
    Newer,       // 원격이 더 최신 (로컬이 구버전)
    LocalOnly,   // 로컬에만 있음(원격 미설치) → 신규 설치 가능
    RemoteOnly,  // 원격에만 있음(로컬 없음)
}

/// 플러그인 한 줄 (스캔 결과)
#[derive(Clone)]
pub struct WpPluginRow {
    pub slug: String,
    pub name: String,
    pub local_ver: String,
    pub remote_ver: String,
    pub active: bool,
    pub diff: WpDiff,
    pub local_size: u64,
    pub remote_size: u64,
    pub local_mtime: i64,
    pub remote_mtime: i64,
}

/// 버전 문자열을 숫자 벡터로 (사전식 비교용). "1.10.0" → [1,10,0]
fn ver_key(v: &str) -> Vec<u64> {
    v.split(|c: char| !c.is_ascii_digit()).filter(|s| !s.is_empty()).filter_map(|s| s.parse().ok()).collect()
}

/// 플러그인 헤더 필드 파싱 (`* Version: 1.2.3` / `Version: 1.2.3`)
fn wp_header_field(head: &str, field: &str) -> Option<String> {
    let f = field.to_ascii_lowercase();
    for line in head.lines() {
        let l = line.trim_start_matches(|c: char| c == '*' || c == '/' || c == '#' || c.is_whitespace());
        if let Some((k, v)) = l.split_once(':') {
            if k.trim().to_ascii_lowercase() == f {
                let val = v.trim().to_string();
                if !val.is_empty() { return Some(val); }
            }
        }
    }
    None
}

/// 파일 앞부분 n 바이트를 문자열로 (플러그인 헤더 읽기용)
fn wp_read_head(path: &std::path::Path, n: usize) -> String {
    use std::io::Read;
    if let Ok(mut f) = std::fs::File::open(path) {
        let mut buf = vec![0u8; n];
        if let Ok(read) = f.read(&mut buf) {
            buf.truncate(read);
            return String::from_utf8_lossy(&buf).to_string();
        }
    }
    String::new()
}

/// 디렉터리 전체 바이트 합계 + 최신 파일 수정시각(epoch초). 심링크는 따라가지 않음.
fn dir_size_mtime(path: &std::path::Path) -> (u64, i64) {
    let (mut size, mut mtime) = (0u64, 0i64);
    let mut stack = vec![path.to_path_buf()];
    while let Some(p) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&p) else { continue };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            let name = e.file_name();
            let nm = name.to_string_lossy();
            if UPLOAD_EXCLUDES.contains(&nm.as_ref()) { continue; } // node_modules/.git 등 제외
            if ft.is_dir() {
                stack.push(e.path());
            } else if ft.is_file() {
                if let Ok(md) = e.metadata() {
                    size += md.len();
                    if let Ok(mt) = md.modified() {
                        if let Ok(d) = mt.duration_since(std::time::UNIX_EPOCH) {
                            let s = d.as_secs() as i64;
                            if s > mtime { mtime = s; }
                        }
                    }
                }
            }
        }
    }
    (size, mtime)
}

/// 로컬 dev/wp/wp-content/{plugins|themes}/* 의 (slug, 이름, 버전, 바이트, 최신수정시각)
fn wp_read_local_assets(src: &str, kind: WpAssetKind) -> Vec<(String, String, String, u64, i64)> {
    let dir = std::path::Path::new(src).join("wp-content").join(kind.subdir());
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(&dir) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        if !p.is_dir() { continue; }
        let slug = e.file_name().to_string_lossy().to_string();
        if slug.starts_with('.') { continue; }
        let (mut name, mut ver) = (String::new(), String::new());
        match kind {
            // 테마: style.css 헤더에서 Theme Name / Version
            WpAssetKind::Theme => {
                let head = wp_read_head(&p.join("style.css"), 8192);
                if let Some(n) = wp_header_field(&head, kind.name_field()) {
                    name = n;
                    ver = wp_header_field(&head, "Version").unwrap_or_default();
                }
            }
            // 플러그인: 메인 .php 파일 헤더에서 Plugin Name / Version
            WpAssetKind::Plugin => {
                if let Ok(files) = std::fs::read_dir(&p) {
                    for f in files.flatten() {
                        let fp = f.path();
                        if fp.extension().and_then(|x| x.to_str()) != Some("php") { continue; }
                        let head = wp_read_head(&fp, 8192);
                        if let Some(n) = wp_header_field(&head, kind.name_field()) {
                            name = n;
                            ver = wp_header_field(&head, "Version").unwrap_or_default();
                            break;
                        }
                    }
                }
            }
        }
        if name.is_empty() { name = slug.clone(); }
        let (size, mtime) = dir_size_mtime(&p);
        out.push((slug, name, ver, size, mtime));
    }
    out
}

/// wp-load.php 를 기준으로 WEBROOT 를 자동 탐지하는 bash/sh 스니펫.
/// 사전조건: 스크립트에 VUSER, DOMAIN 이 이미 설정돼 있어야 한다.
/// 서버마다 웹루트가 다른 문제 해결 — 설정된 site_path(1순위) → 흔한 후보들 → find 폴백 순으로
/// wp-load.php 가 있는 디렉터리를 WEBROOT 로 잡는다. 못 찾으면 WEBROOT 는 빈 문자열.
fn wp_webroot_detect(site_path: &str) -> String {
    let sp = site_path.trim().trim_end_matches('/');
    format!(
        "SITEPATH={sp}\n\
         WEBROOT=''\n\
         for c in \"$SITEPATH\" \"/home/$VUSER/web/$DOMAIN/public_html\" \"/home/$VUSER/public_html\" \"/var/www/$DOMAIN/public_html\" \"/var/www/$DOMAIN\" \"/var/www/html\"; do\n\
           if [ -n \"$c\" ] && [ -f \"$c/wp-load.php\" ]; then WEBROOT=\"$c\"; break; fi\n\
         done\n\
         if [ -z \"$WEBROOT\" ]; then F=\"$(find /home/$VUSER /var/www -maxdepth 5 -name wp-load.php 2>/dev/null | head -1)\"; [ -n \"$F\" ] && WEBROOT=\"$(dirname \"$F\")\"; fi\n",
        sp = sq(sp),
    )
}

/// CMS 무관 docroot(public_html) 자동 탐지 스니펫 (디렉터리 존재 기준).
/// wp-load.php 를 요구하지 않으므로 Rhymix/그누보드/정적 사이트에도 쓸 수 있다.
/// 사전조건: VUSER, DOMAIN 이 이미 설정돼 있어야 한다. 설정된 site_path(1순위) → 흔한 후보 순.
fn docroot_detect(site_path: &str) -> String {
    let sp = site_path.trim().trim_end_matches('/');
    format!(
        "SITEPATH={sp}\n\
         WEBROOT=''\n\
         for c in \"$SITEPATH\" \"/home/$VUSER/web/$DOMAIN/public_html\" \"/home/$VUSER/public_html\" \"/var/www/$DOMAIN/public_html\" \"/var/www/$DOMAIN\" \"/var/www/html\"; do\n\
           if [ -n \"$c\" ] && [ -d \"$c\" ]; then WEBROOT=\"$c\"; break; fi\n\
         done\n",
        sp = sq(sp),
    )
}

/// 로컬 dev/wp 와 대상 사이트의 설치 플러그인/테마 버전을 비교해 표 데이터를 만든다 (blocking).
pub fn wp_asset_scan(server: &Site, c: &CmsInstall, src_base: &str, domain_name: &str, use_root: bool, kind: WpAssetKind) -> Result<Vec<WpPluginRow>, String> {
    cms_validate(server, c, use_root, false, false)?;
    let src = src_base.trim().trim_end_matches('/');
    let sub = kind.subdir();
    let lbl = kind.label();
    if src.is_empty() { return Err("WordPress 소스 경로가 비어 있습니다 (설정 > 서버 SSH 아래 'WordPress 소스')".into()); }
    if !std::path::Path::new(src).join("wp-content").join(sub).is_dir() {
        return Err(format!("로컬 {lbl} 폴더가 없습니다: {src}/wp-content/{sub}"));
    }
    let local = wp_read_local_assets(src, kind);
    let domain = to_ascii_domain(domain_name);
    // 원격: 각 폴더 크기/최신수정 + wp-cli {plugin|theme} list(JSON). wp-cli 없으면 설치.
    // 성능: 예전엔 폴더마다 du(전체순회)+find(전체순회)로 2×N번 트리를 훑었다.
    //       이제 {sub} 디렉터리 전체를 find 한 번으로 훑고 awk 로 폴더별 합계/최신수정을 집계한다(프로세스 1개).
    let raw = format!(
        "export PATH=\"$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin\"\nVUSER={u}\nDOMAIN={d}\n\
         {det}\
         WPCLI=/usr/local/bin/wp\n\
         if [ -z \"$WEBROOT\" ] || [ ! -f \"$WEBROOT/wp-load.php\" ]; then echo 'NOTWP'; exit 0; fi\n\
         DIR=\"$WEBROOT/wp-content/{sub}\"\n\
         if [ -d \"$DIR\" ]; then\n\
           find \"$DIR\" -mindepth 2 {fx}-type f -printf '%P\\t%s\\t%T@\\n' 2>/dev/null | \\\n\
             awk -F'\\t' '{{ i=index($1,\"/\"); if(i>0){{ s=substr($1,1,i-1); b[s]+=$2; t=$3+0; if(t>m[s])m[s]=t }} }} \\\n\
                          END{{ for(k in b) printf \"SIZE|%s|%d|%d\\n\", k, b[k], m[k] }}'\n\
         fi\n\
         if ! [ -x \"$WPCLI\" ]; then curl -fsSL https://raw.githubusercontent.com/wp-cli/builds/gh-pages/phar/wp-cli.phar -o \"$WPCLI\" 2>/dev/null && chmod +x \"$WPCLI\"; fi\n\
         sudo -u \"$VUSER\" \"$WPCLI\" --path=\"$WEBROOT\" {wc} list --fields=name,status,version --format=json 2>/dev/null || echo '[]'\n",
        u = sq(c.hestia_user.trim()), d = sq(&domain), det = wp_webroot_detect(&server.path), sub = sub, wc = kind.wp_cli(), fx = find_exclude_conds(),
    );
    let (script, sshpass, mut env) = eondcms_exec(server, &raw, use_root, c.sudo);
    env.push(("SSHPASS".to_string(), sshpass));
    let out = run_capture(&script, &env)?;
    if out.contains("NOTWP") {
        return Err(format!("대상 사이트에 WordPress 가 설치돼 있지 않습니다 (/home/{}/web/{}/public_html)", c.hestia_user.trim(), domain));
    }
    // 원격 크기/수정시각 (SIZE|slug|bytes|mtime)
    use std::collections::BTreeMap;
    let mut rsize: BTreeMap<String, (u64, i64)> = BTreeMap::new();
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix("SIZE|") {
            let mut it = rest.splitn(3, '|');
            if let (Some(s), Some(b), Some(m)) = (it.next(), it.next(), it.next()) {
                rsize.insert(s.to_string(), (b.trim().parse().unwrap_or(0), m.trim().parse().unwrap_or(0)));
            }
        }
    }
    // 원격 wp-cli JSON (버전/활성)
    let json_line = out.lines().rev().find(|l| l.trim_start().starts_with('[')).unwrap_or("[]");
    let remote: Vec<(String, String, bool)> = match serde_json::from_str::<serde_json::Value>(json_line.trim()) {
        Ok(serde_json::Value::Array(arr)) => arr.iter().filter_map(|it| {
            let slug = it.get("name")?.as_str()?.to_string();
            let ver = it.get("version").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let active = it.get("status").and_then(|v| v.as_str()).map(|s| s == "active" || s == "active-network").unwrap_or(false);
            Some((slug, ver, active))
        }).collect(),
        _ => Vec::new(),
    };
    // 병합
    let mut map: BTreeMap<String, WpPluginRow> = BTreeMap::new();
    for (slug, name, ver, size, mtime) in local {
        map.insert(slug.clone(), WpPluginRow { slug, name, local_ver: ver, remote_ver: String::new(), active: false, diff: WpDiff::LocalOnly, local_size: size, remote_size: 0, local_mtime: mtime, remote_mtime: 0 });
    }
    // 원격 폴더가 존재하는 모든 슬러그(SIZE) 를 반영 — wp-cli 목록에 없어도 폴더가 있으면 잡힘
    for (slug, (sz, mt)) in &rsize {
        map.entry(slug.clone())
            .and_modify(|r| { r.remote_size = *sz; r.remote_mtime = *mt; })
            .or_insert_with(|| WpPluginRow { slug: slug.clone(), name: slug.clone(), local_ver: String::new(), remote_ver: String::new(), active: false, diff: WpDiff::RemoteOnly, local_size: 0, remote_size: *sz, local_mtime: 0, remote_mtime: *mt });
    }
    for (slug, rver, active) in remote {
        map.entry(slug.clone())
            .and_modify(|r| { r.remote_ver = rver.clone(); r.active = active; })
            .or_insert_with(|| WpPluginRow { slug: slug.clone(), name: slug.clone(), local_ver: String::new(), remote_ver: rver.clone(), active, diff: WpDiff::RemoteOnly, local_size: 0, remote_size: 0, local_mtime: 0, remote_mtime: 0 });
    }
    let mut rows: Vec<WpPluginRow> = map.into_values().map(|mut r| {
        let local_present = r.local_size > 0 || !r.local_ver.is_empty();
        let remote_present = r.remote_size > 0 || !r.remote_ver.is_empty();
        r.diff = if local_present && !remote_present { WpDiff::LocalOnly }
            else if !local_present { WpDiff::RemoteOnly }
            else if !r.local_ver.is_empty() && !r.remote_ver.is_empty() {
                // 버전 둘 다 있으면 버전 비교
                match ver_key(&r.local_ver).cmp(&ver_key(&r.remote_ver)) {
                    std::cmp::Ordering::Greater => WpDiff::Update,
                    std::cmp::Ordering::Equal => WpDiff::Same,
                    std::cmp::Ordering::Less => WpDiff::Newer,
                }
            } else {
                // 버전 정보 부족 → 최신수정시각으로 비교
                if r.local_mtime > r.remote_mtime { WpDiff::Update }
                else if r.remote_mtime > r.local_mtime { WpDiff::Newer }
                else { WpDiff::Same }
            };
        r
    }).collect();
    // 정렬: 업데이트가능 → 신규 → 나머지, 그다음 이름
    let rank = |d: WpDiff| match d { WpDiff::Update => 0, WpDiff::LocalOnly => 1, WpDiff::Newer => 2, WpDiff::Same => 3, WpDiff::RemoteOnly => 4 };
    rows.sort_by(|a, b| rank(a.diff).cmp(&rank(b.diff)).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(rows)
}

/// 선택 플러그인/테마를 로컬 dev/wp/wp-content/{plugins|themes} → 사이트로 업로드(동기화).
/// rx 업로드와 동일: /tmp 스테이징 rsync → sudo 로 webroot 에 전체교체 복사 + chown.
pub fn build_wp_asset_upload(server: &Site, c: &CmsInstall, src_base: &str, items: &[String], domain_name: &str, use_root: bool, kind: WpAssetKind) -> Result<Job, String> {
    cms_validate(server, c, use_root, false, false)?;
    let src = src_base.trim().trim_end_matches('/');
    let sub = kind.subdir();
    let lbl = kind.label();
    if src.is_empty() { return Err("WordPress 소스 경로가 비어 있습니다 (설정 > 서버 SSH 아래 'WordPress 소스')".into()); }
    let assetroot = std::path::Path::new(src).join("wp-content").join(sub);
    if !assetroot.is_dir() { return Err(format!("로컬 {lbl} 폴더가 없습니다: {src}/wp-content/{sub}")); }
    let mut names: Vec<&str> = Vec::new();
    for p in items { let p = p.trim(); if p.is_empty() { continue; }
        if !is_safe_name(p) { return Err(format!("{lbl} 이름 형식 오류(영숫자/._- 만): {p}")); } names.push(p); }
    if names.is_empty() { return Err(format!("업로드할 {lbl}을(를) 하나 이상 선택하세요")); }
    let domain = to_ascii_domain(domain_name);
    let vuser = c.hestia_user.trim();
    if vuser.is_empty() { return Err("HestiaCP 유저가 비어 있습니다".into()); }
    let staging = format!("/tmp/hm-wp-{}", store::sanitize(&domain));
    let webroot = format!("/home/{vuser}/web/{domain}/public_html");
    let sshopt = ssh_e(server);
    let user = server.login_id(use_root);
    let host = server.ip.trim();
    let assetbase = format!("{src}/wp-content/{sub}");

    let mut rs = String::new();
    rs.push_str("set -e\n");
    rs.push_str(&format!("echo '== WordPress {lbl} 업로드 (로컬 → 스테이징): {domain} =='\n"));
    // 직접 sshpass→ssh 호출: ssh 옵션은 분리된 단어여야 함
    rs.push_str(&format!("sshpass -e {ssh} {u}@{h} {rm}\n",
        ssh = sshopt, u = sq(user), h = sq(host), rm = remote_cmd(&format!("rm -rf {}", sq(&staging)))));
    for n in &names {
        let local = format!("{assetbase}/{n}");
        rs.push_str(&format!(
            "[ -d {lq} ] || {{ echo '✗ 로컬 {lbl} 없음: {l}'; exit 1; }}\n\
             echo '↑ {sub}/{n}'\n\
             sshpass -e rsync -az --delete --mkpath {excl}-e {ssh} {lq}/ {u}@{h}:{st}/{sub}/{n}/\n",
            lq = sq(&local), l = local, n = n, sub = sub, lbl = lbl, excl = rsync_exclude_flags(),
            ssh = sq(&sshopt), u = sq(user), h = sq(host), st = sq(&staging),
        ));
    }
    let remote_raw = format!(
        "set -e\nexport PATH=\"$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin\"\n\
         VUSER={vu}\nDOMAIN={d}\nSTAGING={st}\n\
         {det}\
         [ -n \"$WEBROOT\" ] && [ -f \"$WEBROOT/wp-load.php\" ] || {{ echo \"✗ WordPress 웹루트 탐지 실패(설정>서버의 '경로'를 지정하거나 대상 확인)\"; rm -rf \"$STAGING\"; exit 1; }}\n\
         echo \"== WordPress {lbl} 설치: $DOMAIN (webroot=$WEBROOT) ==\"\n\
         mkdir -p \"$WEBROOT/wp-content/{sub}\"\n\
         for d in \"$STAGING/{sub}\"/*/; do [ -d \"$d\" ] || continue; n=$(basename \"$d\");\n\
           rm -rf \"$WEBROOT/wp-content/{sub}/$n\"; cp -a \"$d\" \"$WEBROOT/wp-content/{sub}/$n\"; echo \"  + {sub}/$n\"; done\n\
         chown -R \"$VUSER:$VUSER\" \"$WEBROOT/wp-content/{sub}\"\n\
         rm -rf \"$STAGING\"\n\
         echo \"== 업로드 완료 ==\"\n",
        vu = sq(vuser), d = sq(&domain), st = sq(&staging), det = wp_webroot_detect(&server.path), sub = sub, lbl = lbl,
    );
    let (sudo_script, sshpass, env) = eondcms_exec(server, &remote_raw, use_root, c.sudo);
    let script = format!("{rs}{sudo_script}");
    Ok(Job {
        title: format!("WordPress {lbl} 업로드 : {domain_name}"),
        script,
        sshpass,
        env,
        note: format!("로컬 {assetbase} → {user}@{host}:<웹루트 자동탐지>/wp-content/{sub}  (기본 {webroot}) ({})", names.join(",")),
    })
}

/// 선택 플러그인/테마를 사이트 wp-content/{plugins|themes} → 로컬 dev/wp 로 내려받기(동기화).
/// 업로드의 역방향: 원격에서 sudo 로 /tmp 스테이징에 복사(로그인 유저 소유로 chown) → 로컬로 rsync pull → 스테이징 정리.
pub fn build_wp_asset_download(server: &Site, c: &CmsInstall, src_base: &str, items: &[String], domain_name: &str, use_root: bool, kind: WpAssetKind) -> Result<Job, String> {
    cms_validate(server, c, use_root, false, false)?;
    let src = src_base.trim().trim_end_matches('/');
    let sub = kind.subdir();
    let lbl = kind.label();
    if src.is_empty() { return Err("WordPress 소스 경로가 비어 있습니다 (설정 > 서버 SSH 아래 'WordPress 소스')".into()); }
    let assetroot = std::path::Path::new(src).join("wp-content").join(sub);
    if !assetroot.is_dir() { return Err(format!("로컬 {lbl} 폴더가 없습니다: {src}/wp-content/{sub}")); }
    let mut names: Vec<&str> = Vec::new();
    for p in items { let p = p.trim(); if p.is_empty() { continue; }
        if !is_safe_name(p) { return Err(format!("{lbl} 이름 형식 오류(영숫자/._- 만): {p}")); } names.push(p); }
    if names.is_empty() { return Err(format!("내려받을 {lbl}을(를) 하나 이상 선택하세요")); }
    let domain = to_ascii_domain(domain_name);
    let vuser = c.hestia_user.trim();
    if vuser.is_empty() { return Err("HestiaCP 유저가 비어 있습니다".into()); }
    let staging = format!("/tmp/hm-wpdl-{}", store::sanitize(&domain));
    let webroot = format!("/home/{vuser}/web/{domain}/public_html");
    let sshopt = ssh_e(server);
    let user = server.login_id(use_root);
    let host = server.ip.trim();
    let assetbase = format!("{src}/wp-content/{sub}");

    // ① 원격 스테이징 (sudo/root): webroot/{sub}/{name} → /tmp 스테이징에 복사 후 로그인 유저 소유로 chown
    let namelist = names.iter().map(|n| sq(n)).collect::<Vec<_>>().join(" ");
    let remote_raw = format!(
        "set -e\nexport PATH=\"$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin\"\n\
         VUSER={vu}\nDOMAIN={d}\nSTAGING={st}\nLOGINU={lu}\n\
         {det}\
         [ -n \"$WEBROOT\" ] && [ -f \"$WEBROOT/wp-load.php\" ] || {{ echo \"✗ WordPress 웹루트 탐지 실패(설정>서버의 '경로'를 지정하거나 대상 확인)\"; exit 1; }}\n\
         echo \"== WordPress {lbl} 내려받기 준비: $DOMAIN (webroot=$WEBROOT) ==\"\n\
         rm -rf \"$STAGING\"; mkdir -p \"$STAGING/{sub}\"\n\
         for n in {namelist}; do\n\
           if [ -d \"$WEBROOT/wp-content/{sub}/$n\" ]; then cp -a \"$WEBROOT/wp-content/{sub}/$n\" \"$STAGING/{sub}/$n\"; echo \"  ↓준비 {sub}/$n\"; \
           else echo \"  ⚠ 원격에 없어 건너뜀: {sub}/$n\"; fi\n\
         done\n\
         chown -R \"$LOGINU\" \"$STAGING\"\n\
         echo \"== 준비 완료 ==\"\n",
        vu = sq(vuser), d = sq(&domain), st = sq(&staging), lu = sq(user), det = wp_webroot_detect(&server.path), sub = sub, lbl = lbl, namelist = namelist,
    );
    let (stage_script, sshpass, env) = eondcms_exec(server, &remote_raw, use_root, c.sudo);

    // ② 로컬 rsync pull (원격 스테이징 → 로컬 소스). 원격에 없어 스테이징 안 된 항목은 test -d 로 건너뜀.
    let mut rs = String::new();
    rs.push_str("set -e\n");
    rs.push_str(&format!("echo '== WordPress {lbl} 내려받기 (원격 → 로컬): {domain} =='\n"));
    for n in &names {
        let local = format!("{assetbase}/{n}");
        let remote_item = format!("{staging}/{sub}/{n}");
        rs.push_str(&format!(
            "if sshpass -e {ssh} {u}@{h} {test}; then \
               echo '↓ {sub}/{n}'; mkdir -p {lq}; \
               sshpass -e rsync -az --delete --mkpath {excl}-e {sshq} {u}@{h}:{st}/{sub}/{n}/ {lq}/; \
             else echo '⚠ 원격에 없어 건너뜀: {sub}/{n}'; fi\n",
            ssh = sshopt, sshq = sq(&sshopt), u = sq(user), h = sq(host),
            test = remote_cmd(&format!("test -d {}", sq(&remote_item))),
            n = n, sub = sub, excl = rsync_exclude_flags(), lq = sq(&local), st = sq(&staging),
        ));
    }
    // ③ 원격 스테이징 정리 (chown 후 로그인 유저 소유이므로 sudo 불필요)
    rs.push_str(&format!("sshpass -e {ssh} {u}@{h} {rm}\n",
        ssh = sshopt, u = sq(user), h = sq(host), rm = remote_cmd(&format!("rm -rf {}", sq(&staging)))));
    rs.push_str("echo '== 내려받기 완료 =='\n");

    // 스테이징(①)이 실패하면 로컬 rsync(②)로 진행하지 않도록 앞단에 set -e.
    let script = format!("set -e\n{stage_script}\n{rs}");
    Ok(Job {
        title: format!("WordPress {lbl} 내려받기 : {domain_name}"),
        script,
        sshpass,
        env,
        note: format!("원격 {user}@{host}:<웹루트 자동탐지>/wp-content/{sub} → 로컬 {assetbase}  (기본 {webroot}) ({})", names.join(",")),
    })
}

/// 단일 도메인 권한/소유권 진단 (읽기전용). "access denied" 원인 파악용:
/// 실행권한(root/sudo), vuser 존재, 웹루트 탐지, wp-content 소유·권한, vuser 쓰기가능, wp-cli 실행권한.
pub fn build_perm_check(server: &Site, c: &CmsInstall, domain_name: &str, use_root: bool) -> Result<Job, String> {
    cms_validate(server, c, use_root, false, false)?;
    let domain = to_ascii_domain(domain_name);
    let vuser = c.hestia_user.trim();
    if vuser.is_empty() { return Err("HestiaCP 유저가 비어 있습니다".into()); }
    let raw = format!(
        "export PATH=\"$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin\"\n\
         VUSER={vu}\nDOMAIN={d}\n\
         echo \"== 권한 점검: $DOMAIN (웹유저=$VUSER) ==\"\n\
         echo \"- 실행 계정: $(id -un 2>/dev/null) uid=$(id -u 2>/dev/null)\"\n\
         if [ \"$(id -u)\" = 0 ]; then echo \"  ✓ root 권한 확보\"; else echo \"  ✗ root 아님 — 소유권 변경/타유저 접근이 제한될 수 있음(설정에서 루트 또는 sudo 계정 확인)\"; fi\n\
         if id \"$VUSER\" >/dev/null 2>&1; then echo \"  ✓ 시스템 유저 존재: $VUSER\"; else echo \"  ✗ 시스템 유저 없음: $VUSER (HestiaCP 유저/계정 확인)\"; fi\n\
         {det}\
         if [ -z \"$WEBROOT\" ]; then echo \"  ✗ 웹루트(public_html) 자동탐지 실패 — 설정>서버의 '경로' 지정 필요\"; echo \"== 점검 종료 ==\"; exit 0; fi\n\
         echo \"  ✓ 웹루트: $WEBROOT\"\n\
         CMS=\"일반/미상\"\n\
         if [ -f \"$WEBROOT/wp-load.php\" ]; then CMS=\"WordPress\"; elif [ -f \"$WEBROOT/config/config.inc.php\" ] || [ -d \"$WEBROOT/modules\" ]; then CMS=\"Rhymix/XE\"; elif [ -f \"$WEBROOT/common.php\" ] && [ -d \"$WEBROOT/bbs\" ]; then CMS=\"그누보드\"; fi\n\
         echo \"  - CMS: $CMS\"\n\
         OWN=$(stat -c '%U:%G' \"$WEBROOT\" 2>/dev/null); PERM=$(stat -c '%a' \"$WEBROOT\" 2>/dev/null)\n\
         echo \"  - public_html  소유=$OWN  권한(chmod)=$PERM\"\n\
         case \"$OWN\" in \"$VUSER:\"*) echo \"    ✓ 소유자 OK\";; *) echo \"    ✗ 소유자가 $VUSER 아님 (access denied 흔한 원인 — '권한 수정'으로 교정)\";; esac\n\
         if sudo -u \"$VUSER\" test -w \"$WEBROOT\" 2>/dev/null; then echo \"    ✓ 웹유저 쓰기가능\"; else echo \"    ✗ 웹유저 쓰기불가\"; fi\n\
         FOREIGN=$(find \"$WEBROOT\" ! -user \"$VUSER\" -print -quit 2>/dev/null)\n\
         if [ -n \"$FOREIGN\" ]; then echo \"    ✗ 타유저(주로 root) 소유 파일 있음 (예: $FOREIGN)\"; else echo \"    ✓ 모든 파일 $VUSER 소유\"; fi\n\
         echo \"- 쓰기 필요한 하위 디렉터리:\"\n\
         for P in wp-content wp-content/uploads files config data cache; do\n\
           D=\"$WEBROOT/$P\"; [ -d \"$D\" ] || continue\n\
           O=$(stat -c '%U' \"$D\" 2>/dev/null); PM=$(stat -c '%a' \"$D\" 2>/dev/null)\n\
           if sudo -u \"$VUSER\" test -w \"$D\" 2>/dev/null; then W=\"쓰기OK\"; else W=\"✗쓰기불가\"; fi\n\
           echo \"  - $P  소유=$O  권한=$PM  $W\"\n\
         done\n\
         echo \"- 정상 기준: 디렉터리 755 · 파일 644 · public_html 751 · 소유자=웹유저 (files/config/data 등 쓰기폴더도 소유자만 맞으면 755로 충분)\"\n\
         echo \"== 점검 완료 ==\"\n",
        vu = sq(vuser), d = sq(&domain), det = docroot_detect(&server.path),
    );
    let (script, sshpass, env) = eondcms_exec(server, &raw, use_root, c.sudo);
    Ok(Job {
        title: format!("권한 점검 : {domain_name}"),
        script,
        sshpass,
        env,
        note: "읽기전용 진단 — public_html 소유/권한(chmod)/쓰기가능 (CMS 무관)".into(),
    })
}

/// 단일 도메인 소유권 자동 수정: docroot(public_html) 소유 chown -R + 디렉터리 755·파일 644·최상위 751.
/// root/sudo 권한 필요(파일이 root 소유라 웹유저가 못 고치는 경우 대응). 되돌리기 어려운 변경 → 확인 후 실행.
pub fn build_perm_fix(server: &Site, c: &CmsInstall, domain_name: &str, use_root: bool) -> Result<Job, String> {
    cms_validate(server, c, use_root, false, false)?;
    let domain = to_ascii_domain(domain_name);
    let vuser = c.hestia_user.trim();
    if vuser.is_empty() { return Err("HestiaCP 유저가 비어 있습니다".into()); }
    let raw = format!(
        "set -e\nexport PATH=\"$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin\"\n\
         VUSER={vu}\nDOMAIN={d}\n\
         if [ \"$(id -u)\" != 0 ]; then echo \"✗ root 아님 — 소유권 변경 불가('루트로 실행' 켜거나 sudo 계정 확인)\"; exit 1; fi\n\
         if ! id \"$VUSER\" >/dev/null 2>&1; then echo \"✗ 시스템 유저 없음: $VUSER\"; exit 1; fi\n\
         {det}\
         if [ -z \"$WEBROOT\" ]; then echo \"✗ 웹루트(public_html) 탐지 실패 — 설정>서버의 '경로' 지정 필요\"; exit 1; fi\n\
         echo \"== 권한 수정: $WEBROOT → 소유 $VUSER:$VUSER, 디렉터리 755 / 파일 644 / 최상위 751 ==\"\n\
         BEFORE=$(find \"$WEBROOT\" ! -user \"$VUSER\" 2>/dev/null | wc -l)\n\
         echo \"  - 수정 전 타유저 소유 파일: ${{BEFORE}}개\"\n\
         chown -R \"$VUSER:$VUSER\" \"$WEBROOT\"\n\
         echo \"  · chown 완료, chmod 정규화 중...\"\n\
         find \"$WEBROOT\" -type d -print0 2>/dev/null | xargs -0 -r chmod 755\n\
         find \"$WEBROOT\" -type f -print0 2>/dev/null | xargs -0 -r chmod 644\n\
         chmod 751 \"$WEBROOT\"\n\
         AFTER=$(find \"$WEBROOT\" ! -user \"$VUSER\" 2>/dev/null | wc -l)\n\
         echo \"  ✓ 완료 (남은 타유저 소유: ${{AFTER}}개) — 디렉터리 755·파일 644·public_html 751\"\n\
         echo \"== 권한 수정 완료 ==\"\n",
        vu = sq(vuser), d = sq(&domain), det = docroot_detect(&server.path),
    );
    let (script, sshpass, env) = eondcms_exec(server, &raw, use_root, c.sudo);
    Ok(Job {
        title: format!("권한 수정(chown) : {domain_name}"),
        script,
        sshpass,
        env,
        note: format!("public_html 소유 {vuser} + 디렉터리 755·파일 644·최상위 751 (root)"),
    })
}

/// 계정 glob 검증 후 `/home/<acct|*>/web/*/public_html` 패턴을 만든다.
fn perm_glob(account: Option<&str>) -> Result<String, String> {
    Ok(match account {
        Some(a) => {
            let a = a.trim();
            if !is_safe_name(a) { return Err("계정 이름 형식 오류".into()); }
            format!("/home/{a}/web/*/public_html")
        }
        None => "/home/*/web/*/public_html".to_string(),
    })
}

/// 전체(또는 계정) 사이트 일괄 권한 점검 (읽기전용, CMS 무관). 각 사이트 public_html 의 소유자가
/// 웹유저인지, 타유저(주로 root) 소유 파일이 섞였는지, 웹유저 쓰기가능한지 + chmod 를 한 줄 판정.
/// account=Some 이면 그 계정만, None 이면 모든 계정.
pub fn build_perm_audit(s: &Settings, account: Option<&str>) -> Result<Job, String> {
    let srv = ssh_admin_site(s)?;
    let glob = perm_glob(account)?;
    let raw = format!(
        "export PATH=\"$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin\"\n\
         shopt -s nullglob 2>/dev/null || true\n\
         echo \"== 사이트 권한 점검 (public_html 소유/권한 · CMS 무관) ==\"\n\
         TOTAL=0; BAD=0\n\
         for WR in {glob}; do\n\
           [ -d \"$WR\" ] || continue\n\
           OWN=$(echo \"$WR\" | awk -F/ '{{print $3}}')\n\
           DOM=$(basename \"$(dirname \"$WR\")\")\n\
           TOTAL=$((TOTAL+1))\n\
           DOWN=$(stat -c '%U' \"$WR\" 2>/dev/null); PERM=$(stat -c '%a' \"$WR\" 2>/dev/null)\n\
           FOREIGN=$(find \"$WR\" ! -user \"$OWN\" -print -quit 2>/dev/null)\n\
           if sudo -u \"$OWN\" test -w \"$WR\" 2>/dev/null; then WOK=1; else WOK=0; fi\n\
           if [ \"$DOWN\" = \"$OWN\" ] && [ -z \"$FOREIGN\" ] && [ \"$WOK\" = 1 ]; then\n\
             echo \"  ✓ $OWN / $DOM  (소유=$DOWN 권한=$PERM)\"\n\
           else\n\
             BAD=$((BAD+1))\n\
             MSG=\"\"; [ \"$DOWN\" != \"$OWN\" ] && MSG=\"$MSG 소유=$DOWN\"; [ -n \"$FOREIGN\" ] && MSG=\"$MSG 타유저파일있음\"; [ \"$WOK\" != 1 ] && MSG=\"$MSG 쓰기불가\"\n\
             echo \"  ✗ $OWN / $DOM  권한=$PERM :$MSG\"\n\
           fi\n\
         done\n\
         echo \"== 완료: 사이트 $TOTAL곳 · 문제 $BAD곳 (✗ 는 access denied 위험 — '권한 수정'으로 교정) ==\"\n",
        glob = glob,
    );
    let (script, sshpass, env) = eondcms_exec(&srv, &raw, false, true);
    let title = match account { Some(a) => format!("권한 점검(계정) : {a}"), None => "권한 점검(전체 사이트)".to_string() };
    Ok(Job { title, script, sshpass, env, note: "읽기전용 — 각 사이트 public_html 소유/권한(chmod)/쓰기 판정".into() })
}

/// 전체(또는 계정) 사이트 일괄 소유권 수정. 각 사이트 public_html 소유 chown -R + 디렉터리 755·파일 644·최상위 751.
/// 되돌리기 어려운 변경 → 확인 후 실행.
pub fn build_perm_fix_audit(s: &Settings, account: Option<&str>) -> Result<Job, String> {
    let srv = ssh_admin_site(s)?;
    let glob = perm_glob(account)?;
    let raw = format!(
        "export PATH=\"$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin\"\n\
         shopt -s nullglob 2>/dev/null || true\n\
         echo \"== 사이트 권한 일괄 수정 (public_html → 각 계정 소유, 디렉터리 755·파일 644·최상위 751) ==\"\n\
         OK=0; NG=0\n\
         for WR in {glob}; do\n\
           [ -d \"$WR\" ] || continue\n\
           OWN=$(echo \"$WR\" | awk -F/ '{{print $3}}')\n\
           DOM=$(basename \"$(dirname \"$WR\")\")\n\
           id \"$OWN\" >/dev/null 2>&1 || {{ echo \"  ✗ $OWN / $DOM : 유저 없음\"; NG=$((NG+1)); continue; }}\n\
           if chown -R \"$OWN:$OWN\" \"$WR\" 2>/dev/null; then\n\
             find \"$WR\" -type d -print0 2>/dev/null | xargs -0 -r chmod 755\n\
             find \"$WR\" -type f -print0 2>/dev/null | xargs -0 -r chmod 644\n\
             chmod 751 \"$WR\" 2>/dev/null\n\
             echo \"  ✓ $OWN / $DOM\"; OK=$((OK+1))\n\
           else echo \"  ✗ $OWN / $DOM : chown 실패\"; NG=$((NG+1)); fi\n\
         done\n\
         echo \"== 완료: 수정 $OK곳 · 실패 $NG곳 ==\"\n",
        glob = glob,
    );
    let (script, sshpass, env) = eondcms_exec(&srv, &raw, false, true);
    let title = match account { Some(a) => format!("권한 수정(계정) : {a}"), None => "권한 수정(전체 사이트)".to_string() };
    Ok(Job { title, script, sshpass, env, note: "각 사이트 public_html 소유 chown -R + 디렉터리 755·파일 644·최상위 751 (root)".into() })
}

/// 전체(또는 계정) 사이트의 public_html 소유/권한을 조회 (읽기전용, blocking).
/// 반환: (계정, 도메인, "소유자:chmod")  예: ("rokmc", "hbphoto.kr", "root:751")
pub fn scan_site_perms(s: &Settings, account: Option<&str>) -> Result<Vec<(String, String, String)>, String> {
    let srv = ssh_admin_site(s)?;
    let glob = perm_glob(account)?;
    let raw = format!(
        "export PATH=\"$PATH:/usr/bin:/bin:/usr/local/bin\"\n\
         shopt -s nullglob 2>/dev/null || true\n\
         for WR in {glob}; do\n\
           [ -d \"$WR\" ] || continue\n\
           OWN=$(echo \"$WR\" | awk -F/ '{{print $3}}')\n\
           DOM=$(basename \"$(dirname \"$WR\")\")\n\
           PO=$(stat -c '%U' \"$WR\" 2>/dev/null); PM=$(stat -c '%a' \"$WR\" 2>/dev/null)\n\
           echo \"$OWN|$DOM|$PO:$PM\"\n\
         done\n",
        glob = glob,
    );
    let (script, sshpass, mut env) = eondcms_exec(&srv, &raw, false, true);
    env.push(("SSHPASS".to_string(), sshpass));
    let out = run_capture(&script, &env)?;
    let mut res = Vec::new();
    for line in out.lines() {
        let line = line.trim();
        let f: Vec<&str> = line.splitn(3, '|').collect();
        if f.len() == 3 && !f[0].is_empty() && !f[1].is_empty() {
            res.push((f[0].to_string(), f[1].to_string(), f[2].to_string()));
        }
    }
    Ok(res)
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
    let diag_body = "echo '== HOSTMOVER 접속 성공 =='; id 2>/dev/null; echo \"shell=$0\"; echo \"PATH=$PATH\"; \
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
                [ -n \"$WROOT\" ] && echo \"입력한 사이트 path(웹루트): $WROOT\" || echo '사이트 path(웹루트) 미입력 → 홈 디렉터리 하위만 탐색'; \
                for base in \"$WROOT\" \"$WROOT/httpdocs\" \"$WROOT/html\" \"$WROOT/public_html\" \"$R\" \"$R/httpdocs\" \"$R/html\" \"$R/public_html\" \"$R/www\" .; do \
                  [ -n \"$base\" ] || continue; \
                  for f in wp-config.php files/config/config.php files/config/db.config.php data/dbconfig.php config.php; do \
                    if [ -f \"$base/$f\" ]; then hit=1; echo \"CONFIG: $base/$f\"; \
                      grep -iE \"DB_NAME|DB_USER|DB_PASSWORD|DB_HOST|db_database|db_userid|db_password|db_hostname|G5_MYSQL|mysql_host|mysql_user|mysql_password|mysql_db|master\" \"$base/$f\" 2>/dev/null; \
                    fi; done; done; \
                if [ \"$hit\" = 0 ]; then \
                  if [ -z \"$WROOT\" ]; then echo '✗ CMS 설정파일 못 찾음 — 사이트 path(웹루트)가 비어 있습니다. 사이트 설정에서 path(예: /home/사용자/web/도메인/public_html)를 입력하면 그 폴더에서 wp-config.php 등을 찾아 DB 정보를 자동 표시합니다.'; \
                  else echo \"✗ CMS 설정파일 못 찾음 — 입력한 path=$WROOT 및 홈 하위에서 못 찾음. path가 wp-config.php 가 실제로 있는 웹루트인지 확인하세요(끝에 /public_html 등 누락 주의).\"; fi; \
                fi; \
                echo '-- 포트(8000~8099) 사용현황 / 추천 빈 포트 --'; \
                ss -ltn 2>/dev/null | grep -oE ':80[0-9][0-9]' | sort -u | tr '\\n' ' '; echo; \
                for p in $(seq 8002 8099); do ss -ltn 2>/dev/null | grep -q \":$p \" || { echo \"추천 빈 포트: $p\"; break; }; done; \
                echo '(탐색 끝)'";
    // 사이트 path(웹루트)를 WROOT 로 주입 → CMS/DB 설정 탐색이 거기서도 wp-config.php 등을 찾는다.
    let diag = format!("WROOT={}; {}", sq(site.path.trim().trim_end_matches('/')), diag_body);
    let script = format!(
        "sshpass -e {ssh} {user}@{host} {remote}",
        ssh = ssh_e(site),
        user = sq(site.login_id(use_root)),
        host = sq(site.ip.trim()),
        remote = remote_cmd(&diag),
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
            if asis.db_name.trim().is_empty() { return Err("현재 사이트 DB 이름이 비어 있습니다 — 사이트 설정의 DB 항목(db_name)을 입력하세요".into()); }
            let out = dir.join(format!("db_{}.sql.gz", epoch_secs()));
            let remote = mysqldump_cmd(asis);
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
            if tobe.db_name.trim().is_empty() { return Err("신규 사이트 DB 이름이 비어 있습니다 — 사이트 설정의 DB 항목(db_name)을 입력하세요".into()); }
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
            if asis.path.trim().is_empty() { return Err("현재 사이트 경로(path·웹루트)가 비어 있습니다 — 사이트 설정에서 웹루트(예: /home/사용자/web/도메인/public_html)를 입력하세요".into()); }
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
            if tobe.path.trim().is_empty() { return Err("신규 사이트 경로(path·웹루트)가 비어 있습니다 — 사이트 설정에서 웹루트(예: /home/사용자/web/도메인/public_html)를 입력하세요".into()); }
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
            if asis.db_name.trim().is_empty() { return Err("현재 사이트 DB 이름이 비어 있습니다 — 사이트 설정의 DB 항목(db_name)을 입력하세요".into()); }
            if tobe.db_name.trim().is_empty() { return Err("신규 사이트 DB 이름이 비어 있습니다 — 사이트 설정의 DB 항목(db_name)을 입력하세요".into()); }
            let dump = mysqldump_cmd(asis);
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
            if asis.path.trim().is_empty() { return Err("현재 사이트 경로(path·웹루트)가 비어 있습니다 — 사이트 설정에서 웹루트(예: /home/사용자/web/도메인/public_html)를 입력하세요".into()); }
            if tobe.path.trim().is_empty() { return Err("신규 사이트 경로(path·웹루트)가 비어 있습니다 — 사이트 설정에서 웹루트(예: /home/사용자/web/도메인/public_html)를 입력하세요".into()); }
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
    fn rx_upload_valid_bash_and_validation() {
        let mut server = sample_site();
        server.root_id = "tong".into();
        server.root_pw = "tongpw".into();
        let c = CmsInstall { kind: CmsKind::Rhymix, hestia_user: "eond".into(), sudo: true, ..Default::default() };
        // 소스 폴더 준비 (modules/mymod, layouts/mylay)
        let base = std::env::temp_dir().join("hm-rxsrc-test");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("modules/mymod")).unwrap();
        std::fs::create_dir_all(base.join("layouts/my_lay")).unwrap();
        let src = base.to_string_lossy().to_string();
        // 정상: 모듈+레이아웃 지정 → bash 유효
        let j = build_rx_upload(&server, &c, &src, &["mymod".into()], &["my_lay".into()], "ex.com", true).unwrap();
        assert!(j.script.contains("modules/mymod"), "{}", j.script);
        assert!(j.script.contains("layouts/my_lay"));
        assert!(j.script.contains("--exclude=node_modules"), "node_modules 제외 누락:\n{}", j.script);
        // 회귀방지: 직접 sshpass→ssh 호출은 ssh 옵션이 따옴표 없이 분리돼야 함(통째 quote 면 sshpass 가 실행파일명 오인).
        assert!(j.script.contains("sshpass -e ssh -p"), "직접 ssh 호출이 분리되지 않음:\n{}", j.script);
        assert!(!j.script.contains("sshpass -e 'ssh"), "ssh 가 통째로 quote 됨(버그):\n{}", j.script);
        let out = std::process::Command::new("bash").args(["-n", "-c", &j.script]).output().expect("bash");
        assert!(out.status.success(), "bash 구문 오류:\n{}\n{}", String::from_utf8_lossy(&out.stderr), j.script);
        // 빈 선택 → 에러
        assert!(build_rx_upload(&server, &c, &src, &[], &[], "ex.com", true).is_err());
        // 이름 주입 시도 → 에러
        assert!(build_rx_upload(&server, &c, &src, &["../etc".into()], &[], "ex.com", true).is_err());
        // 소스 폴더 없음 → 에러
        assert!(build_rx_upload(&server, &c, "/no/such/hm-src", &["mymod".into()], &[], "ex.com", true).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn wp_plugin_upload_valid_bash() {
        let mut server = sample_site();
        server.root_id = "tong".into();
        server.root_pw = "tongpw".into();
        let c = CmsInstall { kind: CmsKind::WordPress, hestia_user: "eond".into(), sudo: true, ..Default::default() };
        let base = std::env::temp_dir().join("hm-wpsrc-test");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("wp-content/plugins/eond-admin")).unwrap();
        let src = base.to_string_lossy().to_string();
        let j = build_wp_asset_upload(&server, &c, &src, &["eond-admin".into()], "ex.com", true, WpAssetKind::Plugin).unwrap();
        assert!(j.script.contains("wp-content/plugins/eond-admin"), "{}", j.script);
        assert!(j.script.contains("--exclude=node_modules"), "node_modules 제외 누락:\n{}", j.script);
        assert!(j.script.contains("sshpass -e ssh -p"), "직접 ssh 분리 안됨:\n{}", j.script);
        assert!(!j.script.contains("sshpass -e 'ssh"), "ssh 통째 quote(버그):\n{}", j.script);
        let out = std::process::Command::new("bash").args(["-n", "-c", &j.script]).output().expect("bash");
        assert!(out.status.success(), "bash 오류:\n{}\n{}", String::from_utf8_lossy(&out.stderr), j.script);
        // 빈 선택 / 주입 / 소스없음 → 에러
        assert!(build_wp_asset_upload(&server, &c, &src, &[], "ex.com", true, WpAssetKind::Plugin).is_err());
        assert!(build_wp_asset_upload(&server, &c, &src, &["../x".into()], "ex.com", true, WpAssetKind::Plugin).is_err());
        assert!(build_wp_asset_upload(&server, &c, "/no/such/wp", &["eond-admin".into()], "ex.com", true, WpAssetKind::Plugin).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn wp_theme_upload_valid_bash() {
        let mut server = sample_site();
        server.root_id = "tong".into();
        server.root_pw = "tongpw".into();
        let c = CmsInstall { kind: CmsKind::WordPress, hestia_user: "eond".into(), sudo: true, ..Default::default() };
        let base = std::env::temp_dir().join("hm-wpthemesrc-test");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("wp-content/themes/eond-theme")).unwrap();
        let src = base.to_string_lossy().to_string();
        let j = build_wp_asset_upload(&server, &c, &src, &["eond-theme".into()], "ex.com", true, WpAssetKind::Theme).unwrap();
        assert!(j.script.contains("wp-content/themes/eond-theme"), "{}", j.script);
        assert!(j.script.contains("--exclude=node_modules"), "node_modules 제외 누락:\n{}", j.script);
        assert!(j.script.contains("sshpass -e ssh -p"), "직접 ssh 분리 안됨:\n{}", j.script);
        assert!(!j.script.contains("wp-content/plugins"), "테마 업로드에 plugins 경로 혼입:\n{}", j.script);
        let out = std::process::Command::new("bash").args(["-n", "-c", &j.script]).output().expect("bash");
        assert!(out.status.success(), "bash 오류:\n{}\n{}", String::from_utf8_lossy(&out.stderr), j.script);
        // 빈 선택 / 주입 / 테마폴더 없음 → 에러
        assert!(build_wp_asset_upload(&server, &c, &src, &[], "ex.com", true, WpAssetKind::Theme).is_err());
        assert!(build_wp_asset_upload(&server, &c, &src, &["../x".into()], "ex.com", true, WpAssetKind::Theme).is_err());
        assert!(build_wp_asset_upload(&server, &c, "/no/such/wp", &["eond-theme".into()], "ex.com", true, WpAssetKind::Theme).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn wp_asset_download_valid_bash() {
        let mut server = sample_site();
        server.root_id = "tong".into();
        server.root_pw = "tongpw".into();
        let c = CmsInstall { kind: CmsKind::WordPress, hestia_user: "eond".into(), sudo: true, ..Default::default() };
        let base = std::env::temp_dir().join("hm-wpdl-test");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("wp-content/themes")).unwrap();
        std::fs::create_dir_all(base.join("wp-content/plugins")).unwrap();
        let src = base.to_string_lossy().to_string();
        // 테마 내려받기
        let j = build_wp_asset_download(&server, &c, &src, &["eond-theme".into()], "ex.com", true, WpAssetKind::Theme).unwrap();
        assert!(j.script.contains("내려받기"), "{}", j.script);
        assert!(j.script.contains("hm-wpdl-"), "다운로드 전용 스테이징 경로 누락:\n{}", j.script);
        assert!(j.script.contains("wp-content/themes/eond-theme"), "{}", j.script);
        assert!(j.script.contains("chown -R \"$LOGINU\""), "로그인 유저 chown 누락:\n{}", j.script);
        assert!(j.script.contains("test -d"), "원격 존재 확인 가드 누락:\n{}", j.script);
        let out = std::process::Command::new("bash").args(["-n", "-c", &j.script]).output().expect("bash");
        assert!(out.status.success(), "bash 오류:\n{}\n{}", String::from_utf8_lossy(&out.stderr), j.script);
        // 플러그인 내려받기도 유효
        let jp = build_wp_asset_download(&server, &c, &src, &["hello".into()], "ex.com", true, WpAssetKind::Plugin).unwrap();
        assert!(jp.script.contains("wp-content/plugins/hello"), "{}", jp.script);
        let outp = std::process::Command::new("bash").args(["-n", "-c", &jp.script]).output().expect("bash");
        assert!(outp.status.success(), "bash 오류(plugin):\n{}\n{}", String::from_utf8_lossy(&outp.stderr), jp.script);
        // 빈 선택 / 주입 / 소스없음 → 에러
        assert!(build_wp_asset_download(&server, &c, &src, &[], "ex.com", true, WpAssetKind::Theme).is_err());
        assert!(build_wp_asset_download(&server, &c, &src, &["../x".into()], "ex.com", true, WpAssetKind::Theme).is_err());
        assert!(build_wp_asset_download(&server, &c, "/no/such/wp", &["eond-theme".into()], "ex.com", true, WpAssetKind::Theme).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn perm_check_and_audit_valid_bash() {
        let mut server = sample_site();
        server.root_id = "tong".into();
        server.root_pw = "tongpw".into();
        let c = CmsInstall { kind: CmsKind::WordPress, hestia_user: "eond".into(), sudo: true, ..Default::default() };
        // 단일 도메인 권한 점검(CMS 무관 · public_html)
        let j = build_perm_check(&server, &c, "ex.com", true).unwrap();
        assert!(j.script.contains("권한 점검"), "{}", j.script);
        assert!(j.script.contains("public_html") && j.script.contains("chmod"), "{}", j.script);
        let o = std::process::Command::new("bash").args(["-n", "-c", &j.script]).output().expect("bash");
        assert!(o.status.success(), "bash 오류(check):\n{}\n{}", String::from_utf8_lossy(&o.stderr), j.script);
        // 단일 도메인 소유권 수정
        let jf = build_perm_fix(&server, &c, "ex.com", true).unwrap();
        assert!(jf.script.contains("chown -R"), "{}", jf.script);
        let of = std::process::Command::new("bash").args(["-n", "-c", &jf.script]).output().expect("bash");
        assert!(of.status.success(), "bash 오류(fix):\n{}\n{}", String::from_utf8_lossy(&of.stderr), jf.script);
        // 전체 / 계정 일괄 점검 + 수정
        let st = Settings { ssh_host: "1.2.3.4".into(), ssh_user: "tong".into(), ssh_pass: "pw".into(), ..Default::default() };
        for (job, needle) in [
            (build_perm_audit(&st, None).unwrap(), "/home/*/web/*/public_html"),
            (build_perm_audit(&st, Some("eond")).unwrap(), "/home/eond/web/*/public_html"),
            (build_perm_fix_audit(&st, None).unwrap(), "chown -R"),
            (build_perm_fix_audit(&st, Some("eond")).unwrap(), "/home/eond/web/*/public_html"),
        ] {
            assert!(job.script.contains(needle), "'{needle}' 누락:\n{}", job.script);
            let out = std::process::Command::new("bash").args(["-n", "-c", &job.script]).output().expect("bash");
            assert!(out.status.success(), "bash 오류:\n{}\n{}", String::from_utf8_lossy(&out.stderr), job.script);
        }
        // 계정 이름 주입 방지
        assert!(build_perm_audit(&st, Some("../etc")).is_err());
        assert!(build_perm_fix_audit(&st, Some("../etc")).is_err());
    }

    #[test]
    fn wp_read_local_themes_from_style_css() {
        let base = std::env::temp_dir().join("hm-wpthemeread-test");
        let _ = std::fs::remove_dir_all(&base);
        let theme_dir = base.join("wp-content/themes/sample-theme");
        std::fs::create_dir_all(&theme_dir).unwrap();
        std::fs::write(theme_dir.join("style.css"),
            "/*\nTheme Name: Sample Theme\nVersion: 2.3.1\n*/\nbody{}\n").unwrap();
        let src = base.to_string_lossy().to_string();
        let out = wp_read_local_assets(&src, WpAssetKind::Theme);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, "sample-theme");
        assert_eq!(out[0].1, "Sample Theme");
        assert_eq!(out[0].2, "2.3.1");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn wp_ver_key_ordering() {
        assert!(ver_key("1.2.0") < ver_key("1.10.0"));
        assert!(ver_key("2.0") > ver_key("1.99.9"));
        assert_eq!(ver_key("3.4.5"), vec![3, 4, 5]);
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
            let st = Settings { ssh_host: "10.0.0.1".into(), ssh_user: "tong".into(), ssh_pass: "tongpw".into(), ..Default::default() };
            for job in [
                build_cms_install(&server, &c, "예시도메인.com", true).unwrap(),
                build_cms_update(&server, &c, &st, "예시도메인.com", true).unwrap(),
                build_cms_install(&server, &rhymix, "예시도메인.com", true).unwrap(),
                build_cms_update(&server, &rhymix, &st, "예시도메인.com", true).unwrap(),
                build_cms_install(&server, &gnu, "예시도메인.com", true).unwrap(),
                build_cms_update(&server, &gnu, &st, "예시도메인.com", true).unwrap(),
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
        // Rhymix 는 헤드리스(무인) 설치 → 관리자 자동생성 위해 admin 비번/이메일 필요
        let r = CmsInstall { kind: CmsKind::Rhymix, hestia_user: "u".into(), db_name: "u_d".into(), db_user: "u_d".into(), db_pass: "p".into(), ..Default::default() };
        assert!(build_cms_install(&server, &r, "ex.com", true).is_err());
        let r_ok = CmsInstall { admin_pass: "apw".into(), admin_email: "a@b.c".into(), ..r.clone() };
        assert!(build_cms_install(&server, &r_ok, "ex.com", true).is_ok());
        // 그누보드는 자체 /install/ 마법사 — admin 불필요, DB만 있으면 Ok
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
