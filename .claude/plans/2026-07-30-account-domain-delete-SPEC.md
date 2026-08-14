# 개발 스펙 — 계정/도메인 서버 삭제 (Codex 작업 지시서)

작성 2026-07-30. **구현 담당: Codex.** 이 문서는 판단이 끝난 확정 스펙이다.
배경·근거는 [2026-07-30-account-domain-delete.md](2026-07-30-account-domain-delete.md) 참고(읽지 않아도 구현 가능).

---

## 0. 작업 규칙

- **이 문서에 없는 판단은 하지 말고 질문한다.** 특히 "안전장치를 간소화해도 될까"류는 전부 금지.
- Phase 1 → 4 **순서대로** 구현하고 **Phase 단위로 커밋**한다. 한 커밋에 몰지 않는다.
- 각 Phase 의 `DoD`(완료 기준)를 통과해야 다음 Phase 로 간다.
- 빌드: `cargo build` / 테스트: `cargo test` / 릴리스: `cargo build --release`
  (clippy 는 이 환경에 없다. 없으면 건너뛴다)
- 새 코드의 주석·UI 문자열은 **한국어**. 기존 코드 톤(간결한 설명체)을 따른다.
- 커밋 메시지도 한국어. 기존 이력 형식을 따른다.

### 절대 규칙 (위반 시 작업 무효)

1. **자격증명을 소스·테스트·예제에 절대 넣지 않는다.** 비밀번호는 기존 방식대로
   `SSHPASS` 환경변수 경유(`eondcms_exec` 가 처리). argv 에 노출 금지.
2. **`rm -rf` 로 웹루트를 직접 지우지 않는다.** 반드시 `v-delete-web-domain` 경유.
   (파일만 지우면 HestiaCP 에 유령 도메인이 남고 nginx 설정도 남는다)
3. **테스트에서 실제 서버에 접속하지 않는다.** 스크립트 문자열 검증 + `bash -n` 까지만.
4. 기존 함수의 시그니처·동작을 바꾸지 않는다. 전부 **신규 추가**로 처리한다.

---

## 1. 리포 컨텍스트

Rust + egui/eframe 0.31.1 데스크탑 앱. SSH 로 HestiaCP 서버를 관리한다.

| 파일 | 역할 |
|---|---|
| `src/ops.rs` | 모든 원격 작업의 **셸 스크립트 조립**. `build_*` 함수가 `Job` 을 반환 |
| `src/app.rs` | egui UI. 버튼 → 플래그 → `build_*` 호출 → 확인 모달 또는 즉시 실행 |
| `src/model.rs` | `Settings`, `Site` 등 데이터 |

### 실행 모델

```rust
pub struct Job {
    pub title: String,
    pub script: String,       // 로컬에서 실행될 완성된 셸 명령
    pub sshpass: String,      // SSHPASS 로 넘길 비밀번호 (argv 노출 방지)
    pub env: Vec<(String, String)>,
    pub note: String,         // 확인 모달·로그에 보여줄 안내
}
```

`build_*` 는 원격에서 돌 **raw 셸 문자열**을 만들고 마지막에 감싼다:

```rust
let (script, sshpass, env) = eondcms_exec(&srv, &raw, false, true);
//                                         srv, raw, use_root=false, sudo=true
```

`sudo=true` → 원격에서 `sudo -S -p '' bash -s` 로 실행되므로 **raw 안은 root 권한**이다.
`raw` 는 단일 인용 heredoc 안에 들어가므로 로컬 셸 확장이 일어나지 않는다.

### 재사용할 기존 헬퍼 (시그니처 그대로)

```rust
fn ssh_admin_site(s: &Settings) -> Result<Site, String>              // ops.rs:2490
fn server_ssh_site(s: &Settings) -> Result<(Site, String), String>   // ops.rs:1126  (Site, "user@host")
fn eondcms_exec(server: &Site, raw: &str, use_root: bool, sudo: bool)
        -> (String, String, Vec<(String, String)>)                   // ops.rs:515
fn sq(s: &str) -> String                                             // ops.rs:161  셸 단일인용 이스케이프
fn is_safe_name(s: &str) -> bool                                     // ops.rs:2428
fn to_ascii_domain(d: &str) -> String                                // ops.rs:249  한글도메인→퓨니코드
```

`is_safe_name` 구현(이미 있음): 비어있지 않고 `..` 없고 `[A-Za-z0-9_.-]` 만.

**`format!` 안의 raw 셸에서는 `{` `}` 를 `{{` `}}` 로 이스케이프해야 한다.**
셸 중괄호가 많으면 `format!` 을 쓰지 않는 `const` 로 빼는 것을 권한다
(기존 `DISK_MONITOR_INSTALL_BODY` 가 그 패턴).

### 참고할 기존 구현

- `build_account_modules_delete` (ops.rs) — 검증 → glob → 루프 → 카운트 보고. **가장 가까운 선례**
- `build_disk_history` (ops.rs) — 읽기 전용 조사 스크립트의 섹션 구성 방식
- `build_local_backup` (ops.rs) — 웹루트+DB덤프를 로컬로 tar.gz. **Phase 2 에서 재사용**
- `CMS_DETECT` (ops.rs const) — CMS별 DB 이름 탐지. **Phase 1 의 DB 탐지에 그대로 활용**
- 계정 관리 UI: `app.rs` 의 `AcctTab` / 사이트 탭(`sel` 체크박스) / 모듈 탭(선택 삭제)

---

## 2. 확정된 결정 (재논의 금지)

| 항목 | 결정 | 이유 |
|---|---|---|
| 삭제 후 사이드바 로컬 기록 | **건드리지 않는다.** 로그로 안내만 | 로컬 기록(휴지통)과 서버 삭제의 경계를 흐리면 사고가 난다 |
| 백업 실패 시 | **삭제 차단.** 무시 옵션 없음 | 무시 옵션은 결국 눌린다 |
| 백업 전 디스크 확인 | `df` 로 여유 확인, 부족하면 중단 | `v-backup-user` 는 계정 크기만큼 쓴다 |
| 일괄 삭제 | **한 번에 하나만** | 사고 규모를 키우지 않는다 |
| 서버 삭제 UI 위치 | 계정 관리 페이지의 **신규 `위험 작업` 탭** | 사이드바에 두면 휴지통과 혼동된다 |
| 조회 방식 | SSH (HestiaCP API 아님) | 파일 크기·DB 크기·DB 공유 검사는 API 로 못 한다 |

### 용어 구분 (UI 문구에 그대로 사용)

- 사이드바 🗑 `삭제` = **로컬 기록만** 휴지통으로. 서버 무관. (기존 기능, 손대지 않는다)
- 계정 관리 `서버에서 완전 삭제` = **서버의 파일·DB·HestiaCP 등록 제거.** 되돌릴 수 없다.

---

## 3. Phase 1 — 점검 (읽기 전용)

지워질 것을 먼저 보여준다. **아무것도 변경하지 않으므로 확인 모달 없이 즉시 실행**한다.

### 3-1. `ops.rs` 신규 함수

```rust
/// 도메인 삭제 사전 점검 (SSH, sudo, 읽기 전용).
/// 지워질 대상과 크기, DB 이름, 그리고 그 DB 를 다른 도메인이 함께 쓰는지 조사한다.
pub fn build_domain_delete_probe(s: &Settings, account: &str, domain: &str) -> Result<Job, String>

/// 계정 삭제 사전 점검 (SSH, sudo, 읽기 전용).
pub fn build_account_delete_probe(s: &Settings, account: &str) -> Result<Job, String>
```

두 함수 공통 검증(맨 앞에서):

```rust
let acct = account.trim();
if !is_safe_name(acct) { return Err("계정 이름 형식 오류 (영숫자/._- 만)".into()); }
if is_protected_account(acct, s) { return Err(format!("보호 대상 계정입니다: {acct}")); }
```

도메인 쪽은 추가로:

```rust
let da = to_ascii_domain(domain);
if !is_safe_name(&da) { return Err(format!("도메인 형식 오류: {domain}")); }
```

### 3-2. `ops.rs` 신규 헬퍼 — 보호 계정

```rust
/// 삭제하면 서버가 죽는 계정. 실수로도 지나가지 못하게 입력 검증 단계에서 막는다.
fn is_protected_account(acct: &str, s: &Settings) -> bool {
    const SYSTEM: &[&str] = &[
        "admin", "root", "www-data", "mysql", "hestiaweb", "hestia",
        "daemon", "bin", "sys", "nobody", "sshd", "systemd-network",
    ];
    let a = acct.trim();
    if a.is_empty() { return true; }
    if SYSTEM.iter().any(|p| p.eq_ignore_ascii_case(a)) { return true; }
    // 설정의 서버 SSH 계정 자신 — 지우면 지금 쓰는 접속 경로가 사라진다
    if !s.ssh_user.trim().is_empty() && s.ssh_user.trim().eq_ignore_ascii_case(a) { return true; }
    false
}
```

### 3-3. 도메인 점검 셸 (raw)

`{u}` = 계정, `{d}` = 퓨니코드 도메인. 둘 다 `sq()` 로 감싸 주입한다.

```sh
set +e
export PATH="$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin"
VBIN=/usr/local/hestia/bin
VUSER={u}
DOMAIN={d}
WR="/home/$VUSER/web/$DOMAIN/public_html"
WEBDIR="/home/$VUSER/web/$DOMAIN"

echo "===== 삭제 사전 점검: $VUSER / $DOMAIN ====="
echo

echo "[1] HestiaCP 등록 상태"
if $VBIN/v-list-web-domains "$VUSER" plain 2>/dev/null | awk '{print $1}' | grep -qx "$DOMAIN"; then
  echo "  ✓ 웹도메인 등록됨"
else
  echo "  ✗ HestiaCP 에 이 웹도메인이 없습니다 — 삭제할 것이 없거나 계정/도메인이 틀렸습니다"
fi
$VBIN/v-list-dns-domains  "$VUSER" plain 2>/dev/null | awk '{print $1}' | grep -qx "$DOMAIN" \
  && echo "  · DNS 도메인 있음 (함께 삭제됩니다)"  || echo "  · DNS 도메인 없음"
$VBIN/v-list-mail-domains "$VUSER" plain 2>/dev/null | awk '{print $1}' | grep -qx "$DOMAIN" \
  && echo "  · 메일 도메인 있음 (메일함까지 함께 삭제됩니다)" || echo "  · 메일 도메인 없음"
echo

echo "[2] 파일"
if [ -d "$WEBDIR" ]; then
  echo "  경로 $WEBDIR"
  echo "  크기 $(du -sh "$WEBDIR" 2>/dev/null | cut -f1)"
  echo "  파일수 $(find "$WEBDIR" -type f 2>/dev/null | wc -l)"
else
  echo "  (디렉터리 없음: $WEBDIR)"
fi
echo

echo "[3] 데이터베이스"
DB=""
if [ -f "$WEBDIR/pythonapp/.env" ]; then
  DB="$(grep -oP 'DATABASE_URL=.*/\K[^?[:space:]]+' "$WEBDIR/pythonapp/.env" 2>/dev/null | head -1)"
  [ -z "$DB" ] && DB="$(grep -oP '^DB_NAME=\K.*' "$WEBDIR/pythonapp/.env" 2>/dev/null | head -1)"
fi
[ -z "$DB" ] && [ -f "$WR/wp-config.php" ] && DB="$(grep -oP "DB_NAME'?\s*,\s*'\K[^']+" "$WR/wp-config.php" 2>/dev/null | head -1)"
for CF in "$WR/files/config/db.config.php" "$WR/config/db.config.php"; do
  [ -n "$DB" ] && break
  [ -f "$CF" ] || continue
  DB="$(grep -oP "'database'\s*=>\s*'\K[^']+" "$CF" 2>/dev/null | head -1)"
  [ -z "$DB" ] && DB="$(grep -oP "db_database'?\s*[=,]\s*[\"']\K[^\"']+" "$CF" 2>/dev/null | head -1)"
done
[ -z "$DB" ] && [ -f "$WR/data/dbconfig.php" ] && DB="$(grep -oP "mysql_db'?\s*[,=]\s*[\"']\K[^\"']+" "$WR/data/dbconfig.php" 2>/dev/null | head -1)"

if [ -z "$DB" ]; then
  echo "  DB 를 찾지 못했습니다 (설정파일 미검출) — DB 는 삭제되지 않습니다"
  echo "HM_DB="
  echo "HM_DBSHARED=0"
else
  SZ=$(mysql -N -B -e "SELECT IFNULL(ROUND(SUM(data_length+index_length)/1024/1024,1),0) FROM information_schema.tables WHERE table_schema='$DB'" 2>/dev/null)
  EX=$(mysql -N -B -e "SHOW DATABASES LIKE '$DB'" 2>/dev/null | grep -c .)
  echo "  이름 $DB   크기 ${SZ:-?}MB   실제존재 $([ "$EX" = 1 ] && echo 예 || echo '아니오(설정만 남음)')"
  if $VBIN/v-list-databases "$VUSER" plain 2>/dev/null | awk '{print $1}' | grep -qx "$DB"; then
    echo "  HestiaCP 등록 ✓ (v-delete-database 로 정상 삭제 가능)"
  else
    echo "  HestiaCP 등록 ✗ — 패널 밖에서 수동 생성된 DB 입니다"
    echo "    → v-delete-database 로는 안 지워집니다. 이 경우 DB 는 건너뜁니다(수동 처리)"
  fi

  echo
  echo "  [공유 검사] 이 DB 를 다른 도메인도 쓰고 있는지 (중요)"
  HITS=0; SHARED=""
  shopt -s nullglob
  for OWR in /home/"$VUSER"/web/*/public_html; do
    [ -d "$OWR" ] || continue
    OD="$(basename "$(dirname "$OWR")")"
    if grep -rqsF "$DB" "$OWR/wp-config.php" "$OWR/data/dbconfig.php" \
         "$OWR/config/db.config.php" "$OWR/files/config/db.config.php" \
         "$(dirname "$OWR")/pythonapp/.env" 2>/dev/null; then
      HITS=$((HITS+1))
      [ "$OD" != "$DOMAIN" ] && SHARED="$SHARED $OD"
    fi
  done
  if [ -n "$SHARED" ]; then
    echo "    ✗✗ 다른 도메인도 이 DB 를 참조합니다:$SHARED"
    echo "       → DB 를 지우면 그 사이트들이 죽습니다. DB 삭제를 건너뛰어야 합니다."
    echo "HM_DBSHARED=1"
  else
    echo "    ✓ 이 도메인만 사용 (참조 $HITS 곳)"
    echo "HM_DBSHARED=0"
  fi
  echo "HM_DB=$DB"
fi
echo

echo "[4] 백업 여유"
df -h /backup 2>/dev/null | tail -1 | sed 's/^/  /' || df -h / | tail -1 | sed 's/^/  /'
echo
echo "===== 점검 끝 — 삭제하려면 백업 후 '서버에서 완전 삭제' ====="
```

**`HM_DB=` / `HM_DBSHARED=` 줄의 목적**: UI 가 로그 출력에서 이 값을 파싱해
삭제 단계에 넘긴다(§5-3). 형식을 바꾸지 말 것.

두 줄은 **모든 분기에서 반드시 출력**된다(DB 미검출 시에도 `HM_DB=` + `HM_DBSHARED=0`).
한쪽만 출력되면 이전 점검의 값이 남아 **다른 도메인의 DB 를 지울 수 있다.**
UI 쪽도 점검을 시작할 때 `danger_db` / `danger_db_shared` 를 먼저 리셋한다(§3-5).

### 3-4. 계정 점검 셸 (raw)

```sh
set +e
export PATH="$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin"
VBIN=/usr/local/hestia/bin
VUSER={u}

echo "===== 계정 삭제 사전 점검: $VUSER ====="
echo
if ! $VBIN/v-list-users plain 2>/dev/null | awk '{print $1}' | grep -qx "$VUSER"; then
  echo "  ✗ HestiaCP 계정이 아닙니다: $VUSER — 중단하세요"
  echo "===== 점검 끝 ====="
  exit 0
fi
echo "[1] 계정 요약"
$VBIN/v-list-user "$VUSER" plain 2>/dev/null | sed 's/^/  /'
echo
echo "[2] 웹 도메인 (전부 삭제됩니다)"
$VBIN/v-list-web-domains "$VUSER" plain 2>/dev/null | awk '{print "  "$1}' | sort
echo "  총 $($VBIN/v-list-web-domains "$VUSER" plain 2>/dev/null | grep -c .) 개"
echo
echo "[3] 데이터베이스 (전부 삭제됩니다)"
$VBIN/v-list-databases "$VUSER" plain 2>/dev/null | awk '{print "  "$1}' | sort
echo
echo "[4] DNS / 메일 도메인"
echo "  DNS  $($VBIN/v-list-dns-domains  "$VUSER" plain 2>/dev/null | grep -c .) 개"
echo "  메일 $($VBIN/v-list-mail-domains "$VUSER" plain 2>/dev/null | grep -c .) 개 (메일함 포함 삭제)"
echo
echo "[5] 디스크"
[ -d "/home/$VUSER" ] && echo "  /home/$VUSER  $(du -sh "/home/$VUSER" 2>/dev/null | cut -f1)"
echo
echo "[6] 백업 여유"
df -h /backup 2>/dev/null | tail -1 | sed 's/^/  /' || df -h / | tail -1 | sed 's/^/  /'
echo
echo "※ v-delete-user 는 도메인·DB·메일·크론·/home 을 모두 지웁니다. 되돌릴 수 없습니다."
echo "===== 점검 끝 ====="
```

### 3-5. UI — `위험 작업` 탭 신설

`app.rs`:

1. `enum AcctTab { Sites, Modules, Notes }` → **`Danger` 추가**
2. 탭 라벨 배열에 `(AcctTab::Danger, "  ⚠ 위험 작업  ")` 추가
3. `App` 에 필드 추가:

```rust
/// 위험작업 탭: 대상 도메인 (빈 문자열이면 계정 전체 작업만 가능)
danger_domain: String,
/// 확인 타이핑 입력값
danger_confirm_text: String,
/// 점검 완료 여부 — 백업/삭제 버튼의 활성 조건
danger_probed: bool,
/// 백업 완료 여부 — 삭제 버튼의 활성 조건
danger_backed_up: bool,
/// 점검에서 파싱한 DB 이름 (빈 문자열 = DB 없음/미검출)
danger_db: String,
/// 점검에서 파싱한 DB 공유 여부 — true 면 DB 삭제를 막는다
danger_db_shared: bool,
```

전부 `App::new()` 에서 `String::new()` / `false` 로 초기화.

4. 탭 내용:

```
[대상] 도메인 콤보박스 (계정 관리 중인 계정의 도메인 목록에서 선택, "계정 전체" 항목 포함)

── 1단계: 점검 ──────────────────────────────
  [ 점검 (읽기 전용) ]        ← 항상 활성
  결과는 아래 로그창에 표시됩니다.

── 2단계: 백업 ──────────────────────────────
  [ 백업 ]                    ← danger_probed 일 때만 활성
  도메인: 파일+DB를 로컬로 / 계정 전체: 서버의 v-backup-user

── 3단계: 서버에서 완전 삭제 ────────────────
  ⚠ 되돌릴 수 없습니다.
  확인을 위해 <도메인 또는 계정명> 을 입력하세요: [        ]
  [ 서버에서 완전 삭제 ]      ← probed && backed_up && 입력일치 일 때만 활성
```

- 3단계 버튼은 빨간색: `egui::Color32::from_rgb(200, 70, 70)` 계열
- 계정 전체 삭제는 이 탭 맨 아래 `egui::CollapsingHeader::new("계정 전체 삭제")`
  안에 넣어 한 번 더 숨긴다(기본 닫힘)
- 대상(도메인/계정)이 바뀌면 `danger_probed`, `danger_backed_up`,
  `danger_confirm_text`, `danger_db`, `danger_db_shared` 를 **모두 리셋**한다.
  (다른 도메인의 점검 결과로 삭제되는 것을 막는다 — 반드시 구현)
- **점검 버튼을 누를 때도** 같은 5개를 먼저 리셋한 뒤 실행한다.
  같은 대상을 재점검했는데 두 번째 출력에 `HM_DB=` 가 없으면 첫 번째 값이 살아남는다.

5. 점검은 확인 모달 없이 `ops::spawn` 으로 직접 실행(읽기 전용). 기존 패턴:

```rust
match ops::build_domain_delete_probe(&self.store.settings, &acct, &dom) {
    Ok(job) => {
        self.running = true; self.last_ok = None;
        self.status = "삭제 사전 점검 중...".into();
        let ctx2 = ctx.clone();
        ops::spawn(job, self.tx.clone(), move || ctx2.request_repaint());
    }
    Err(e) => { self.last_ok = Some(false); self.status = format!("점검 실패: {e}"); self.log.push(format!("삭제 점검: {e}")); }
}
```

### 3-6. Phase 1 테스트

`ops.rs` 의 `mod tests` 에 추가:

```rust
#[test]
fn delete_probe_jobs_valid_bash_and_validation() {
    let st = Settings { ssh_host: "1.2.3.4".into(), ssh_user: "tong".into(), ssh_pass: "pw".into(), ..Default::default() };
    for job in [
        build_domain_delete_probe(&st, "rokmc", "example.com").unwrap(),
        build_account_delete_probe(&st, "rokmc").unwrap(),
    ] {
        let out = std::process::Command::new("bash").args(["-n", "-c", &job.script]).output().expect("bash");
        assert!(out.status.success(), "bash 오류({}):\n{}", job.title, String::from_utf8_lossy(&out.stderr));
    }
    // 점검은 읽기 전용 — 삭제 명령이 절대 들어가면 안 된다
    for job in [
        build_domain_delete_probe(&st, "rokmc", "example.com").unwrap(),
        build_account_delete_probe(&st, "rokmc").unwrap(),
    ] {
        for bad in ["v-delete-", "DROP DATABASE", "rm -rf"] {
            assert!(!job.script.contains(bad), "점검에 파괴 명령 포함: {bad}");
        }
    }
    // 보호 계정 거부
    for bad in ["admin", "root", "www-data", "mysql", "hestiaweb", "tong"] {  // tong = ssh_user 자신
        assert!(build_account_delete_probe(&st, bad).is_err(), "보호 계정 통과: {bad}");
        assert!(build_domain_delete_probe(&st, bad, "example.com").is_err(), "보호 계정 통과: {bad}");
    }
    // 주입 거부
    assert!(build_account_delete_probe(&st, "../etc").is_err());
    assert!(build_domain_delete_probe(&st, "rokmc", "ex'ample.com").is_err());
    assert!(build_domain_delete_probe(&st, "rokmc", "../../etc").is_err());
}
```

### DoD (Phase 1)

- [ ] `cargo build` 경고 없음, `cargo test` 전부 통과
- [ ] 위 테스트 통과 (특히 **점검 스크립트에 `v-delete-`/`DROP`/`rm -rf` 가 없음**)
- [ ] 위험 작업 탭에서 점검 버튼이 동작하고 로그에 섹션 [1]~[4] 가 출력됨
- [ ] 대상 도메인을 바꾸면 점검/백업 완료 상태가 리셋됨

---

## 4. Phase 2 — 백업

### 4-1. `ops.rs` 신규 함수

```rust
/// 계정 전체 백업 (SSH, sudo). HestiaCP 표준 백업 — 메일·크론·DNS 까지 포함한다.
/// 도메인 단위 백업은 기존 build_local_backup 을 재사용하므로 신규 함수가 없다.
pub fn build_account_backup(s: &Settings, account: &str) -> Result<Job, String>
```

검증은 Phase 1 과 동일(`is_safe_name` + `is_protected_account`).

```sh
set +e
export PATH="$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin"
VBIN=/usr/local/hestia/bin
VUSER={u}
echo "===== 계정 백업: $VUSER ====="

# 계정 크기 vs /backup 여유 — 백업 도중 디스크가 차면 서버 전체가 위험하다
NEED=$(du -sm "/home/$VUSER" 2>/dev/null | cut -f1); [ -z "$NEED" ] && NEED=0
BD=/backup; [ -d "$BD" ] || BD=/
FREE=$(df -Pm "$BD" 2>/dev/null | awk 'NR==2{print $4}'); [ -z "$FREE" ] && FREE=0
echo "  계정 크기 약 ${NEED}MB · $BD 여유 ${FREE}MB"
if [ "$FREE" -lt $((NEED + 1024)) ] 2>/dev/null; then
  echo "  ✗ 여유 공간이 부족합니다 (필요 ${NEED}MB + 여유분 1GB) — 백업을 중단합니다"
  echo "    공간을 확보한 뒤 다시 시도하세요. 백업 없이 삭제하지 마십시오."
  exit 1
fi

$VBIN/v-backup-user "$VUSER"
RC=$?
echo
if [ "$RC" = 0 ]; then
  echo "  ✓ 백업 완료"
  ls -lh /backup/"$VUSER".*.tar 2>/dev/null | tail -3 | sed 's/^/    /'
else
  echo "  ✗ 백업 실패 (v-backup-user=$RC) — 삭제로 넘어가지 마십시오"
  exit 1
fi
echo "===== 백업 끝 ====="
```

### 4-2. UI

- 대상이 **도메인**이면 → 기존 `ops::build_local_backup(&settings, &[(acct, dom)], &dest)` 호출
  (`dest` 는 기존 백업 경로 로직을 그대로 사용)
- 대상이 **계정 전체**면 → `build_account_backup`
- 성공(`last_ok == Some(true)`) 시 `danger_backed_up = true`. 실패 시 `false` 유지.

### DoD (Phase 2)

- [ ] 백업 실패 시 `danger_backed_up` 이 true 가 되지 않아 3단계 버튼이 계속 비활성
- [ ] `bash -n` 통과, 보호 계정 거부 테스트 추가

---

## 5. Phase 3 — 도메인 삭제 (파괴적)

### 5-1. `ops.rs` 신규 함수

```rust
/// 도메인 완전 삭제 (SSH, sudo, 되돌릴 수 없음 — 확인 모달 필수).
/// `db`: 함께 삭제할 DB 이름. 빈 문자열이면 DB 를 건드리지 않는다.
/// 호출 전에 백업이 완료되어 있어야 한다(UI 가 강제).
pub fn build_domain_delete(s: &Settings, account: &str, domain: &str, db: &str) -> Result<Job, String>
```

검증:

```rust
let acct = account.trim();
if !is_safe_name(acct) { return Err("계정 이름 형식 오류".into()); }
if is_protected_account(acct, s) { return Err(format!("보호 대상 계정입니다: {acct}")); }
let da = to_ascii_domain(domain);
if !is_safe_name(&da) { return Err(format!("도메인 형식 오류: {domain}")); }
let dbn = db.trim();
if !dbn.is_empty() && !is_safe_name(dbn) { return Err(format!("DB 이름 형식 오류: {db}")); }
```

### 5-2. 셸 (raw)

```sh
set +e
export PATH="$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin"
VBIN=/usr/local/hestia/bin
VUSER={u}
DOMAIN={d}
DB={db}
LOG=/var/log/hostmover/deletions.log
mkdir -p /var/log/hostmover
TS=$(date '+%F %T')

echo "===== 서버에서 완전 삭제: $VUSER / $DOMAIN ====="

# 등록 확인 — 없으면 아무것도 하지 않는다
if ! $VBIN/v-list-web-domains "$VUSER" plain 2>/dev/null | awk '{print $1}' | grep -qx "$DOMAIN"; then
  echo "  ✗ HestiaCP 에 웹도메인이 없습니다 — 중단 (계정/도메인을 확인하세요)"
  exit 1
fi

SZ=$(du -sh "/home/$VUSER/web/$DOMAIN" 2>/dev/null | cut -f1)
echo "  대상 파일 ${SZ:-?} · DB ${DB:-없음}"
echo

echo "[1] 웹도메인 삭제 (파일·nginx/apache 설정·SSL 포함)"
$VBIN/v-delete-web-domain "$VUSER" "$DOMAIN"
RC=$?
if [ "$RC" != 0 ]; then
  echo "  ✗ 실패 (v-delete-web-domain=$RC) — 이후 단계를 중단합니다"
  echo "$TS FAIL domain $VUSER/$DOMAIN v-delete-web-domain=$RC" >> "$LOG"
  exit 1
fi
echo "  ✓ 완료"

echo "[2] DNS 도메인"
if $VBIN/v-list-dns-domains "$VUSER" plain 2>/dev/null | awk '{print $1}' | grep -qx "$DOMAIN"; then
  $VBIN/v-delete-dns-domain "$VUSER" "$DOMAIN" && echo "  ✓ 삭제" || echo "  ✗ 실패(수동 확인 필요)"
else
  echo "  · 없음"
fi

echo "[3] 메일 도메인"
if $VBIN/v-list-mail-domains "$VUSER" plain 2>/dev/null | awk '{print $1}' | grep -qx "$DOMAIN"; then
  $VBIN/v-delete-mail-domain "$VUSER" "$DOMAIN" && echo "  ✓ 삭제" || echo "  ✗ 실패(수동 확인 필요)"
else
  echo "  · 없음"
fi

echo "[4] 데이터베이스"
if [ -z "$DB" ]; then
  echo "  · 건너뜀 (대상 없음 — 미검출이거나 다른 도메인과 공유 중)"
else
  if $VBIN/v-list-databases "$VUSER" plain 2>/dev/null | awk '{print $1}' | grep -qx "$DB"; then
    $VBIN/v-delete-database "$VUSER" "$DB" && echo "  ✓ $DB 삭제" || echo "  ✗ $DB 삭제 실패(수동 확인 필요)"
  else
    echo "  ✗ $DB 는 HestiaCP 에 등록되어 있지 않습니다 — 건너뜁니다"
    echo "    패널 밖에서 만든 DB 는 자동 삭제하지 않습니다. 필요하면 직접 처리하세요."
  fi
fi
echo

echo "[5] 삭제 검증"
FAILED=0
$VBIN/v-list-web-domains "$VUSER" plain 2>/dev/null | awk '{print $1}' | grep -qx "$DOMAIN" \
  && { echo "  ✗ 패널에 아직 남아 있음"; FAILED=1; } || echo "  ✓ 패널에서 사라짐"
[ -d "/home/$VUSER/web/$DOMAIN" ] \
  && { echo "  ✗ 디렉터리 잔존: /home/$VUSER/web/$DOMAIN"; FAILED=1; } || echo "  ✓ 디렉터리 제거됨"
if [ -n "$DB" ]; then
  if mysql -N -B -e "SHOW DATABASES LIKE '$DB'" 2>/dev/null | grep -q .; then
    echo "  ✗ DB 잔존: $DB"; FAILED=1
  else
    echo "  ✓ DB 제거됨"
  fi
fi

echo "$TS DELETE domain $VUSER/$DOMAIN files=${SZ:-?} db=${DB:-none} verify=$([ "$FAILED" = 0 ] && echo OK || echo INCOMPLETE)" >> "$LOG"
echo
if [ "$FAILED" = 0 ]; then
  echo "===== 삭제 완료 ====="
else
  echo "===== 삭제 불완전 — 위 ✗ 항목을 수동 확인하세요 ====="
fi
echo "※ hostmover 사이드바의 기록은 그대로 남아 있습니다. 필요하면 🗑 로 정리하세요."
echo "※ 이력: $LOG"
```

### 5-3. UI — DB 이름/공유 여부 전달

점검 로그에서 파싱한다. 로그 수신 지점(`self.log.push(...)` 가 일어나는 곳)에서:

```rust
// 삭제 점검 출력에서 DB 정보를 뽑아 3단계로 넘긴다 (형식은 SPEC §3-3 고정)
if let Some(v) = line.strip_prefix("HM_DB=") { self.danger_db = v.trim().to_string(); }
if let Some(v) = line.strip_prefix("HM_DBSHARED=") { self.danger_db_shared = v.trim() == "1"; }
```

삭제 호출 시:

```rust
// 공유 중이면 DB 를 절대 넘기지 않는다 (다른 사이트가 죽는다)
let db = if self.danger_db_shared { "" } else { self.danger_db.as_str() };
ops::build_domain_delete(&self.store.settings, &acct, &dom, db)
```

UI 에 공유 상태를 눈에 보이게 표시한다:

- 공유 아님: `DB rokmc_wp 도 함께 삭제됩니다`
- 공유 중: 주황색 `⚠ 이 DB 는 다른 도메인도 사용 중 — DB 는 삭제하지 않습니다`
- 미검출: `DB 를 찾지 못해 삭제하지 않습니다`

### 5-4. 전용 확인 모달

기존 `eond_confirm_modal` 은 **재사용하지 않는다.** 제목이 "설치 작업 확인" 으로 고정이고
예/아니오만 있어 파괴 작업에 부적합하다.

`app.rs` 에 신규:

```rust
/// 되돌릴 수 없는 삭제 전용 확인 모달. 대상 이름을 정확히 타이핑해야 실행이 열린다.
danger_confirm: Option<(ops::Job, String)>,   // (작업, 타이핑해야 하는 문자열)

fn danger_confirm_modal(&mut self, ctx: &egui::Context) { ... }
```

요구사항:

- 창 제목 `⚠ 되돌릴 수 없는 삭제`
- 본문에 **서버 호스트**(`settings.ssh_host` 또는 `hestia_host`), 대상, `job.note` 표시
- 빨간 경고문: `이 작업은 되돌릴 수 없습니다. 백업에서 복원하는 것만 가능합니다.`
- 타이핑 입력칸. `danger_confirm_text.trim() == 기대문자열` 일 때만 실행 버튼 활성
- 취소 시 `danger_confirm = None`, `danger_confirm_text.clear()`
- 실행 시 기존 패턴대로 `ops::spawn`
- `update()` 에서 `eond_confirm_modal` 을 호출하는 곳 옆에 `danger_confirm_modal` 도 호출

### 5-5. Phase 3 테스트

```rust
#[test]
fn domain_delete_job_safety() {
    let st = Settings { ssh_host: "1.2.3.4".into(), ssh_user: "tong".into(), ssh_pass: "pw".into(), ..Default::default() };
    let j = build_domain_delete(&st, "rokmc", "example.com", "rokmc_wp").unwrap();
    let out = std::process::Command::new("bash").args(["-n", "-c", &j.script]).output().expect("bash");
    assert!(out.status.success(), "bash 오류:\n{}", String::from_utf8_lossy(&out.stderr));
    // 정석 경로를 쓸 것 — 웹루트 직접 rm 금지
    assert!(j.script.contains("v-delete-web-domain"));
    assert!(!j.script.contains("rm -rf /home"), "웹루트 직접 삭제 금지");
    assert!(j.script.contains("v-delete-database"));
    // 검증·이력이 빠지지 않았는지
    assert!(j.script.contains("deletions.log"));
    // DB 를 안 넘기면 DB 삭제 시도가 없어야 한다
    let j2 = build_domain_delete(&st, "rokmc", "example.com", "").unwrap();
    assert!(j2.script.contains("DB=''") || j2.script.contains("DB=\"\""), "빈 DB 주입 형식 확인");
    // 보호 계정·주입 거부
    for bad in ["admin", "root", "tong"] {
        assert!(build_domain_delete(&st, bad, "example.com", "").is_err());
    }
    assert!(build_domain_delete(&st, "rokmc", "example.com", "db'x").is_err());
    assert!(build_domain_delete(&st, "rokmc", "../etc", "").is_err());
}
```

### DoD (Phase 3)

- [ ] 위 테스트 통과
- [ ] 백업 미완료 상태에서 3단계 버튼이 비활성
- [ ] 타이핑이 정확히 일치할 때만 실행 버튼 활성
- [ ] DB 공유 상태에서 삭제하면 스크립트에 DB 가 빈 값으로 전달됨(수동 확인)
- [ ] **테스트 계정으로 실제 1회 검증**(아래 §7) 전에는 실사용 금지 안내를 커밋 메시지에 남긴다

---

## 6. Phase 4 — 계정 삭제 (가장 위험)

### 6-1. `ops.rs`

```rust
/// 계정 완전 삭제 (SSH, sudo, 되돌릴 수 없음). 도메인·DB·메일·크론·/home 전부.
pub fn build_account_delete(s: &Settings, account: &str) -> Result<Job, String>
```

검증은 Phase 1 과 동일 + **`v-list-users` 에 있는지 원격에서 재확인**(셸 안에서).

```sh
set +e
export PATH="$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin"
VBIN=/usr/local/hestia/bin
VUSER={u}
LOG=/var/log/hostmover/deletions.log
mkdir -p /var/log/hostmover
TS=$(date '+%F %T')

echo "===== 계정 완전 삭제: $VUSER ====="

# 원격에서도 한 번 더 막는다 (Rust 검증을 우회한 경우 대비)
case "$VUSER" in
  admin|root|www-data|mysql|hestia|hestiaweb|daemon|bin|sys|nobody)
    echo "  ✗ 보호 대상 계정입니다 — 중단"; exit 1 ;;
esac
if ! $VBIN/v-list-users plain 2>/dev/null | awk '{print $1}' | grep -qx "$VUSER"; then
  echo "  ✗ HestiaCP 계정이 아닙니다 — 중단"; exit 1
fi

ND=$($VBIN/v-list-web-domains "$VUSER" plain 2>/dev/null | grep -c .)
SZ=$(du -sh "/home/$VUSER" 2>/dev/null | cut -f1)
echo "  도메인 ${ND}개 · ${SZ:-?}"
echo

echo "[1] v-delete-user 실행"
$VBIN/v-delete-user "$VUSER"
RC=$?
[ "$RC" = 0 ] && echo "  ✓ 완료" || echo "  ✗ 실패(v-delete-user=$RC)"
echo

echo "[2] 삭제 검증"
FAILED=0
$VBIN/v-list-users plain 2>/dev/null | awk '{print $1}' | grep -qx "$VUSER" \
  && { echo "  ✗ 패널에 계정이 남아 있음"; FAILED=1; } || echo "  ✓ 패널에서 사라짐"
[ -d "/home/$VUSER" ] \
  && { echo "  ✗ /home/$VUSER 잔존"; FAILED=1; } || echo "  ✓ /home 제거됨"
id "$VUSER" >/dev/null 2>&1 \
  && { echo "  ✗ 시스템 사용자 잔존"; FAILED=1; } || echo "  ✓ 시스템 사용자 제거됨"

echo "$TS DELETE user $VUSER domains=$ND size=${SZ:-?} rc=$RC verify=$([ "$FAILED" = 0 ] && echo OK || echo INCOMPLETE)" >> "$LOG"
echo
[ "$FAILED" = 0 ] && echo "===== 삭제 완료 =====" || echo "===== 삭제 불완전 — 위 ✗ 를 수동 확인 ====="
echo "※ hostmover 사이드바 기록은 남아 있습니다."
echo "※ 이력: $LOG"
```

### 6-2. UI

- `CollapsingHeader("계정 전체 삭제")` 안 (기본 닫힘)
- 타이핑 확인 문자열 = **계정명**
- 버튼 문구 `계정을 서버에서 완전 삭제`
- 백업(`v-backup-user`) 완료 필수

### 6-3. 테스트

Phase 3 과 동일 형태로: `bash -n`, `v-delete-user` 포함, `deletions.log` 포함,
보호 계정 전부 거부(`admin`/`root`/`mysql`/`hestiaweb`/`tong`), 주입 거부.
추가로 **원격 측 보호 case 문이 스크립트에 들어있는지** 확인:

```rust
assert!(j.script.contains("보호 대상 계정입니다"), "원격 측 2차 방어 누락");
```

### DoD (Phase 4)

- [ ] 위 테스트 통과, 전체 `cargo test` 통과
- [ ] 릴리스 빌드 경고 없음
- [ ] README 또는 `docs/` 에 이 기능 문서 1개 추가 (`docs/account-delete.md`,
      내용: 두 종류 삭제의 차이 · 3단계 절차 · DB 공유 함정 · 복원 방법)

---

## 7. 실서버 검증 절차 (사람이 수행 — Codex 는 하지 않는다)

Phase 3 완료 후, 실사용 전에 반드시 한 번:

1. HestiaCP 에 테스트 계정 `hmtest` + 도메인 `hmtest.example.com` 생성, WordPress 설치
2. 위험 작업 탭에서 점검 → 백업 → 삭제
3. 확인: 패널에서 도메인 사라짐 · `/home/hmtest/web/...` 없음 · DB 없음 ·
   `/var/log/hostmover/deletions.log` 에 기록 남음 · `/backup/hmtest.*.tar` 존재
4. `v-restore-user` 로 복원이 되는지도 확인(백업이 실제로 쓸 수 있는지)

---

## 8. 흔한 함정 (미리 알림)

1. **`format!` 안의 셸 중괄호** — `${VAR}`, `awk '{print}'` 등이 전부 깨진다.
   중괄호가 많은 스크립트는 `const` 로 빼고 `head + BODY` 로 조립한다.
2. **`v-list-*` 의 `plain` 출력은 탭 구분**이고 첫 필드가 이름이다. `grep -qx` 로
   정확히 일치 비교할 것 — `grep -q` 만 쓰면 `example.com` 이 `sub.example.com` 에도 걸린다.
3. **`du -sh` 는 파일이 지워진 뒤엔 못 쓴다.** 크기는 삭제 **전에** 구해 변수에 담는다.
4. **DB 탐지는 웹도메인 삭제 전에** 해야 한다(파일이 사라지면 설정을 못 읽는다).
   그래서 DB 이름은 점검 단계에서 파싱해 인자로 넘긴다.
5. `set -e` 를 쓰면 `grep -q` 실패에 스크립트가 죽는다. 조사/삭제 스크립트는 `set +e` 로
   시작하고 실패를 직접 검사한다(기존 진단 함수들과 동일).
6. UI 상태 리셋 누락 — 대상을 바꿨는데 이전 점검 결과가 남아 있으면 **다른 도메인의 DB 를
   지울 수 있다.** §3-5 의 리셋을 반드시 넣는다.

---

## 9. 최종 완료 기준

- [ ] Phase 1~4 각 DoD 통과, Phase 별 커밋 4개 이상
- [ ] `cargo test` 전부 통과 / `cargo build --release` 경고 없음
- [ ] 점검 스크립트에 파괴 명령이 **없음**(테스트로 강제)
- [ ] 삭제 스크립트가 `v-delete-*` 를 쓰고 웹루트를 직접 `rm -rf` 하지 **않음**(테스트로 강제)
- [ ] 보호 계정·주입 거부가 Rust 검증과 원격 셸 양쪽에 존재
- [ ] `docs/account-delete.md` 작성
- [ ] PR 본문에 §7 실서버 검증이 **아직 수행되지 않았음**을 명시
