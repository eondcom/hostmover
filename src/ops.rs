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
    fn punycode_korean_domain() {
        let out = idna::domain_to_ascii("한국.kr").unwrap();
        assert!(out.starts_with("xn--"), "got {out}");
        assert!(out.ends_with(".kr"), "got {out}");
    }
}
