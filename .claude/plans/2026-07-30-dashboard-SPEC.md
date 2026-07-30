# 개발 스펙 — 시작 대시보드 (Codex 작업 지시서)

작성 2026-07-30. **구현 담당: Codex.** 판단이 끝난 확정 스펙이다.

목표: 앱을 열면 **서버 헬스**와 **도메인별 헬스**, 그리고 등록된 고객·도메인 현황을
한 화면에서 보고 이상을 감지한다.

---

## 0. 작업 규칙

- **이 문서에 없는 판단은 하지 말고 질문한다.**
- Phase 1 → 4 **순서대로**, **Phase 단위로 커밋**한다. 각 Phase 의 `DoD` 를 통과해야 다음으로 간다.
- 빌드 `cargo build` / 테스트 `cargo test` / 릴리스 `cargo build --release` (clippy 는 이 환경에 없다)
- 주석·UI 문자열은 **한국어**, 기존 코드 톤을 따른다. 커밋 메시지도 한국어.

### 절대 규칙

1. **자격증명을 소스·테스트에 넣지 않는다.** 비밀번호는 `eondcms_exec` 가 `SSHPASS` 로 처리한다.
2. **대시보드가 자동으로 SSH 를 쏘지 않는다.** (§2-1 — 이 기능의 가장 중요한 제약)
3. 기존 함수의 시그니처·동작을 바꾸지 않는다. 전부 신규 추가.
4. 테스트에서 실제 서버·외부 네트워크에 접속하지 않는다. 스크립트 문자열 검증 + `bash -n` 까지만.

---

## 1. 리포 컨텍스트

`src/ops.rs` = 원격 셸 스크립트 조립(`build_*` → `Job`), `src/app.rs` = egui UI,
`src/model.rs` = 영속 데이터.

### 이미 있는 자산 (그대로 재사용한다 — 새로 만들지 말 것)

| 자산 | 위치 | 대시보드에서의 역할 |
|---|---|---|
| `Store.scan_cache: Vec<CachedSite>` | model.rs | **디스크에 저장되는 사이트 스캔 결과.** 즉시 지표의 원천 |
| `Store.scan_cache_at: i64` | model.rs | 캐시 시각 → "N시간 전 기준" 표시 |
| `hydrate_scan_cache()` | app.rs:1734 | 캐시 → `all_sites` 로딩. 이미 시작 시 호출됨 |
| `ago_text(at) -> String` | app.rs:221 | "3시간 전" 문자열 |
| `Store.customers: Vec<Customer>` | model.rs | 고객 수·도메인 수. `deleted_at.is_some()` = 휴지통 |
| `CachedSite{kind,version,status,file_bytes,db_bytes,perm,created}` | model.rs | CMS 분포·용량 합계·업데이트 필요 수 |
| `UiState{view,...}` | model.rs | 마지막 화면 복원. 대시보드 추가 시 확장 |
| `card()`, `btn_primary()`, `grid_label()`, `site_row()` | app.rs | UI 구성 요소 |
| `ops::spawn(job, tx, repaint)` | ops.rs:4224 | 비동기 실행. 결과는 `LogMsg::Line` 스트림 + `Done{ok}` |
| `eondcms_exec(&srv, &raw, false, true)` | ops.rs:515 | raw 셸 → sudo 원격 실행 |
| `ssh_admin_site(s)` / `server_ssh_site(s)` | ops.rs | SSH 대상 `Site` 생성 |

### 결과 수신 방식 — 마커 파싱

`LogMsg` 에는 구조화된 결과 변체가 없다(텍스트 `Line` + `Done{ok}`). 그래서 **스크립트가
`KEY=VALUE` 마커 줄을 출력하고 UI 가 그것을 파싱한다.** 이 방식은 이미 계정 삭제 기능의
`HM_DB=` / `HM_DBSHARED=` 에서 쓰고 있다(`app.rs` 의 `strip_prefix` 참고).

- 서버 스냅샷: `HM_DASH_<KEY>=<VALUE>` (한 줄에 하나)
- 도메인별: `HM_DOMH <계정> <도메인> <LH> <PH> <DNS> <CERT_DAYS> <ROOT> <A레코드>` (공백 구분)

`LogMsg` 에 새 변체를 추가하거나 스레드 구조를 바꾸지 말 것.

---

## 2. 확정된 설계 결정 (재논의 금지)

### 2-1. 2층 구조 — 즉시층과 서버층을 분리한다

**대시보드가 열릴 때 SSH 를 자동으로 쏘면 안 된다.** SSH 한 번이 수 초, 여러 개면 수십 초다.
첫 화면에서 그러면 앱이 멈춘 것처럼 보이고, VPN 미연결·서버 다운 상태에서는 매번 타임아웃을
기다리게 된다. 첫 화면은 **0ms 에 그려져야 한다.**

| 층 | 데이터 출처 | 시점 |
|---|---|---|
| **즉시층** | 로컬(`customers`, `scan_cache`) + 마지막 서버 스냅샷 캐시 | 화면 즉시. SSH 0회 |
| **서버층** | SSH 스냅샷 / 도메인 헬스 | **사용자가 버튼을 누를 때만** |

서버층 결과는 **디스크에 캐시**해서, 다음 실행 때도 "어제 09:12 기준" 으로 보여준다.
`scan_cache` 와 같은 방식이다. 값이 오래됐으면 시각을 회색으로 흐리게 표시한다.

### 2-2. 서버 조회는 스크립트 1개 = SSH 1회

지표별로 SSH 를 나누면 N배 느려진다. 서버 스냅샷은 **단일 스크립트가 모든 값을 모아** 마커로
출력한다(§4). 도메인 헬스도 마찬가지로 스크립트 1개이며, 내부에서 도메인을 **병렬** 검사한다(§5).

### 2-3. 시작 화면

`Settings` 에 `start_view: String` 추가 (`"dashboard"` | `"last"`), **기본값 `"dashboard"`**.
- `"dashboard"` → 잠금 해제 후 항상 대시보드
- `"last"` → 기존 동작(`UiState.view` 복원)

설정 > 연결 탭 하단에 라디오/콤보로 노출한다.

### 2-4. 도메인 헬스 판정 규칙 (가장 중요)

실측에서 확인한 함정: **`--resolve` 로 서버 자신에게 물으면 vhost 매칭이 실패해도 기본 vhost 가
`200` 을 준다.** 존재하지 않는 도메인이 정상으로 보였다. 그래서:

- **로컬 응답(`LH`) 단독으로 "정상" 판정을 하지 않는다.** 참고값으로만 쓴다.
- `DNS=ok` 일 때는 **공개 응답(`PH`)** 이 주 지표다(실제 방문자가 보는 것).
- `DNS=other/none` 이면 그 자체가 이상이고, 이때 `PH` 는 남의 서버 응답이므로 판정에 쓰지 않는다.
- **웹루트 존재(`ROOT`)** 를 함께 본다 — vhost 설정만 남고 파일이 없는 경우를 잡는다.
- 인증서는 **파일에서** 읽는다(`/home/<u>/conf/web/<d>/ssl/<d>.crt`). 네트워크가 필요 없어 훨씬 빠르다.

이 규칙은 §5 스크립트의 `issue_of()` 에 구현돼 있다. **로직을 바꾸지 말 것.**

### 2-5. 감시 상태는 "느슨하게" 표시

디스크 감시(`/etc/cron.d/hm-disk-monitor`)와 트래픽 감시(`/etc/cron.d/hm-traffic-monitor`)는
아직 머지되지 않은 PR 에 속한다. **파일 존재로만 확인**하고 없으면 "미설치"로 표시한다.
그 기능의 Rust 코드를 참조하거나 의존하지 말 것.

---

## 3. Phase 1 — 즉시층 대시보드 (SSH 0회)

### 3-1. `MainView::Dashboard` 추가

`app.rs`:

1. `enum MainView { Domain, Settings, AccountModules(usize), AllSites }` → **`Dashboard` 추가**
2. `UiState.view` 직렬화 문자열에 `"dashboard"` 추가 (저장/복원 양쪽 `match` 에 반영)
3. `update()` 의 `match self.view` 에 `MainView::Dashboard => self.dashboard_page(ctx)` 추가
4. 상단바에 대시보드 버튼 추가(`ph::GAUGE` 또는 `ph::HOUSE`). `AllSites`/`Settings` 버튼과 같은 토글 패턴
5. `App::new()` 의 초기 `view` 는 `Settings.start_view` 에 따라 결정

### 3-2. 즉시 지표 — 로컬 데이터만으로 계산

`app.rs` 에 순수 계산 함수를 두고 매 프레임 호출한다(수백 건이라 비용은 무시 가능):

```rust
/// 대시보드 즉시 지표 — 로컬 데이터만으로 계산한다(SSH 없음).
struct LocalStats {
    customers: usize,        // 활성 고객
    customers_trash: usize,  // 휴지통
    domains: usize,          // 등록 도메인(로컬 기록)
    sites: usize,            // 스캔된 서버 사이트 수
    by_kind: Vec<(String, usize)>, // CMS 분포 (많은 순)
    need_update: usize,      // status 가 업데이트 필요
    file_bytes: u64,
    db_bytes: u64,
    no_cred: usize,          // 자격증명 미입력 도메인
}

fn local_stats(store: &Store) -> LocalStats
```

- `customers`: `!c.deleted_at.is_some()`
- `domains`: 활성 고객의 `domains.len()` 합
- `sites`/`by_kind`/`file_bytes`/`db_bytes`: `store.scan_cache` 집계
- `need_update`: `CachedSite.status` 가 업데이트 필요를 뜻하는 값일 때
  (기존 표시 로직을 확인해 같은 기준을 쓸 것. 새 기준을 만들지 말 것)
- `no_cred`: `asis.ip` 나 `asis.ftp_id` 가 빈 도메인 수

### 3-3. 화면 구성

```
┌─ Hostmover v0.1.0 (커밋 · 날짜) ──────────────── tong@mars.eond.com ─┐
│                                                                      │
│  ┌ 등록 현황 ────────┐ ┌ 서버 사이트 ───────┐ ┌ 주의 ─────────────┐ │
│  │ 고객      12      │ │ 사이트    97       │ │ ⚠ 업데이트 필요 4 │ │
│  │ 도메인    34      │ │ WordPress 41       │ │ ⚠ 자격증명 미입력 2│ │
│  │ 휴지통     1      │ │ Rhymix    38       │ │                   │ │
│  │                   │ │ 그누보드  12       │ │ (없으면 "이상 없음")│ │
│  │                   │ │ 파일 182GB DB 9GB  │ │                   │ │
│  │                   │ │ 3시간 전 기준      │ │                   │ │
│  └───────────────────┘ └────────────────────┘ └───────────────────┘ │
│                                                                      │
│  ┌ 서버 헬스 ───────────────────────────── [새로고침] ────────────┐ │
│  │ (Phase 2)  아직 조회하지 않았습니다 / 또는 캐시 + "어제 09:12"   │ │
│  └────────────────────────────────────────────────────────────────┘ │
│  ┌ 도메인 헬스 ─────────────────────────── [점검] ────────────────┐ │
│  │ (Phase 3)                                                       │ │
│  └────────────────────────────────────────────────────────────────┘ │
│                                                                      │
│  바로가기: [전체 사이트] [일괄 업데이트] [디스크 점검] [설정]        │
└──────────────────────────────────────────────────────────────────────┘
```

- 카드는 기존 `card()` 헬퍼 사용. 3열은 `ui.columns(3, ...)`
- 상단 우측에 SSH 대상(`ssh_user@ssh_host`) 표시. 미설정이면 회색 "서버 SSH 미설정"
- 버전 문자열은 기존 `version_line()` 재사용
- 캐시가 비었으면(`scan_cache.is_empty()`) "아직 스캔하지 않았습니다 — [전체 사이트] 에서 스캔" 안내
- 숫자는 크게(`ui.heading` 급), 라벨은 작게. 용량은 기존 바이트 포맷 헬퍼가 있으면 재사용

### DoD (Phase 1)

- [ ] `cargo build` 경고 없음, `cargo test` 통과
- [ ] 앱 시작 시 대시보드가 먼저 보이고, **SSH 가 한 번도 실행되지 않는다**
- [ ] 캐시가 없어도 빈 화면이 아니라 안내가 보인다
- [ ] 상단바에서 대시보드 ↔ 다른 화면 전환이 되고, 재시작 시 `start_view` 설정이 적용된다

---

## 4. Phase 2 — 서버 헬스 스냅샷 (SSH 1회)

### 4-1. `ops.rs` 신규

```rust
/// 서버 상태 스냅샷 (SSH, sudo, 읽기 전용).
/// 부하·메모리·디스크·서비스·HestiaCP 규모·감시 설치 상태를 한 번에 모은다.
pub fn build_server_snapshot(s: &Settings) -> Result<Job, String>
```

`ssh_admin_site(s)?` → `SERVER_SNAPSHOT_BODY` (아래) → `eondcms_exec(&srv, raw, false, true)`.
`format!` 을 쓰지 않는 `const` 이므로 셸 중괄호를 이스케이프할 필요가 없다.

**아래 스크립트는 실제로 실행 검증했다. 그대로 쓸 것.**

```sh
set +e
export PATH="$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin"
VBIN=/usr/local/hestia/bin
echo "===== 서버 상태 스냅샷 ====="

echo "[호스트]"
echo "  $(hostname) · $(uname -r)"
UP=$(uptime -p 2>/dev/null | sed 's/^up //'); echo "  가동 ${UP:-?}"
echo "HM_DASH_HOST=$(hostname)"
echo "HM_DASH_UPTIME=${UP:-?}"

echo "[부하]"
read L1 L5 L15 REST < /proc/loadavg
CORES=$(nproc 2>/dev/null); [ -z "$CORES" ] && CORES=1
echo "  load $L1 / $L5 / $L15  (코어 ${CORES})"
echo "HM_DASH_LOAD1=$L1"
echo "HM_DASH_LOAD5=$L5"
echo "HM_DASH_CORES=$CORES"

echo "[메모리]"
MT=$(awk '/^MemTotal:/{print int($2/1024)}' /proc/meminfo 2>/dev/null)
MA=$(awk '/^MemAvailable:/{print int($2/1024)}' /proc/meminfo 2>/dev/null)
[ -z "$MT" ] && MT=0; [ -z "$MA" ] && MA=0
MU=$((MT - MA)); MP=0; [ "$MT" -gt 0 ] && MP=$((MU * 100 / MT))
echo "  ${MU}MB / ${MT}MB (${MP}%)"
echo "HM_DASH_MEMPCT=$MP"
echo "HM_DASH_MEMTOTAL=$MT"

echo "[디스크]"
MAXP=0; MAXMP="-"
while read -r FS SZ USED AVAIL PCT MP; do
  # 실제 블록장치만 (efivarfs·tmpfs·overlay 같은 가상 fs 가 최대치를 오염시킨다)
  case "$FS" in /dev/*) ;; *) continue ;; esac
  P=${PCT%\%}
  printf '  %-24s %5s  %5s 남음\n' "$MP" "$PCT" "$AVAIL"
  if [ "$P" -gt "$MAXP" ] 2>/dev/null; then MAXP=$P; MAXMP=$MP; fi
done < <(df -hP 2>/dev/null | awk 'NR>1')
echo "HM_DASH_DISKMAX=$MAXP"
echo "HM_DASH_DISKMAXMP=$MAXMP"

echo "[서비스]"
SVCFAIL=""
for S in nginx apache2 mysql mariadb exim4 dovecot cron; do
  systemctl list-unit-files "$S.service" >/dev/null 2>&1 || continue
  if systemctl is-active --quiet "$S" 2>/dev/null; then
    echo "  ✓ $S"
  else
    echo "  ✗ $S 정지"
    SVCFAIL="$SVCFAIL $S"
  fi
done
for U in $(systemctl list-units --type=service --state=running,failed 'php*-fpm.service' --no-legend 2>/dev/null | awk '{print $1}'); do
  systemctl is-active --quiet "$U" 2>/dev/null || SVCFAIL="$SVCFAIL ${U%.service}"
done
NPHP=$(systemctl list-units --type=service 'php*-fpm.service' --no-legend 2>/dev/null | grep -c .)
NPHPBAD=$(systemctl list-units --type=service --state=failed 'php*-fpm.service' --no-legend 2>/dev/null | grep -c .)
echo "  php-fpm ${NPHP}개 중 실패 ${NPHPBAD}개"
echo "HM_DASH_SVCFAIL=$(echo $SVCFAIL | sed 's/^ *//')"
echo "HM_DASH_PHPFPM=$NPHP"
echo "HM_DASH_PHPFPMBAD=$NPHPBAD"

echo "[HestiaCP]"
if [ -x "$VBIN/v-list-users" ]; then
  NU=$("$VBIN/v-list-users" plain 2>/dev/null | grep -c .)
  ND=0
  for U in $("$VBIN/v-list-users" plain 2>/dev/null | awk '{print $1}'); do
    N=$("$VBIN/v-list-web-domains" "$U" plain 2>/dev/null | grep -c .)
    ND=$((ND + N))
  done
  echo "  계정 ${NU} · 웹도메인 ${ND}"
  echo "HM_DASH_USERS=$NU"
  echo "HM_DASH_DOMAINS=$ND"
else
  echo "  (HestiaCP CLI 없음)"
  echo "HM_DASH_USERS=-"
  echo "HM_DASH_DOMAINS=-"
fi

echo "[자동 감시]"
if [ -f /etc/cron.d/hm-disk-monitor ]; then
  DL=$(cat /var/lib/hm-disk-monitor/last-run 2>/dev/null)
  DR=$(grep -v '^date' /var/lib/hm-disk-monitor/history.tsv 2>/dev/null | tail -1 | awk '{print $2}')
  echo "  디스크 감시 설치됨 · 마지막 ${DL:-기록없음} · 최근판정 ${DR:-?}"
  echo "HM_DASH_DISKMON=1"
  echo "HM_DASH_DISKMON_LAST=${DL:-}"
  echo "HM_DASH_DISKMON_RESULT=${DR:-}"
else
  echo "  디스크 감시 미설치"
  echo "HM_DASH_DISKMON=0"
  echo "HM_DASH_DISKMON_LAST="
  echo "HM_DASH_DISKMON_RESULT="
fi
if [ -f /etc/cron.d/hm-traffic-monitor ]; then
  echo "  트래픽 감시 설치됨"
  echo "HM_DASH_TRAFFICMON=1"
else
  echo "  트래픽 감시 미설치"
  echo "HM_DASH_TRAFFICMON=0"
fi

echo "===== 스냅샷 끝 ====="
```

### 4-2. 결과 저장

`model.rs` 에 추가:

```rust
/// 마지막 서버 스냅샷 (대시보드에서 즉시 표시)
#[derive(Default, Serialize, Deserialize, Clone)]
pub struct ServerSnapshot {
    #[serde(default)] pub at: i64,          // unix 초
    #[serde(default)] pub host: String,
    #[serde(default)] pub uptime: String,
    #[serde(default)] pub load1: String,
    #[serde(default)] pub cores: String,
    #[serde(default)] pub mem_pct: String,
    #[serde(default)] pub disk_max: String,     // 최대 사용률 (%)
    #[serde(default)] pub disk_max_mp: String,   // 그 마운트포인트
    #[serde(default)] pub svc_fail: String,      // 정지된 서비스 목록 (공백 구분)
    #[serde(default)] pub phpfpm: String,
    #[serde(default)] pub phpfpm_bad: String,
    #[serde(default)] pub users: String,
    #[serde(default)] pub domains: String,
    #[serde(default)] pub diskmon: String,       // "1"|"0"
    #[serde(default)] pub diskmon_last: String,
    #[serde(default)] pub diskmon_result: String,
    #[serde(default)] pub trafficmon: String,
}
```

`Store` 에 `#[serde(default)] pub server_snapshot: ServerSnapshot` 추가.

값은 전부 `String` 으로 받는다 — 스크립트가 `-` 나 빈 값을 줄 수 있고, 숫자 파싱 실패로
대시보드가 깨지는 것보다 그대로 보여주는 게 낫다. 색상 판정이 필요한 곳에서만
`.parse::<u32>().unwrap_or(0)` 한다.

### 4-3. UI

- `App` 에 `dash_running: Option<DashRun>` 추가 (`enum DashRun { Snapshot, DomainHealth }`)
  — 어떤 조회가 끝났는지 구분해 결과를 반영한다. 계정 삭제의 `DangerRun` 과 같은 패턴.
- 로그 수신부에서 `HM_DASH_` 마커를 파싱해 임시 버퍼에 모으고, `Done{ok:true}` 시
  `store.server_snapshot` 에 커밋 + `at = now_unix()` + 저장.
- 카드 표시: 부하는 `load1 / cores` 비율이 1.0 초과면 주황, 2.0 초과면 빨강.
  디스크 `disk_max` 90 이상 주황, 95 이상 빨강. `svc_fail` 비어있지 않으면 빨강.
  `phpfpm_bad > 0` 이면 빨강 + "PHP-FPM 진단" 바로가기.
- 조회 중에는 버튼 비활성 + `ui.spinner()`.

### DoD (Phase 2)

- [ ] `bash -n` 통과 테스트, 스냅샷 스크립트에 파괴 명령(`rm -rf`, `v-delete-`, `DROP`) 부재 테스트
- [ ] 조회 후 앱을 재시작해도 값이 남아 있고 "N시간 전" 이 표시된다
- [ ] 서버 SSH 미설정이면 버튼이 비활성이고 안내가 보인다

---

## 5. Phase 3 — 도메인별 헬스 (SSH 1회, 내부 병렬)

### 5-1. `ops.rs` 신규

```rust
/// 도메인별 헬스 점검 (SSH, sudo, 읽기 전용).
/// HestiaCP 웹도메인 전체를 병렬로: 웹루트 존재·서버응답·공개응답·DNS·인증서 만료.
pub fn build_domain_health(s: &Settings) -> Result<Job, String>
```

**아래 스크립트는 실제로 실행 검증했다(오탐 3건을 잡아 수정한 최종본). 판정 로직을 바꾸지 말 것.**

```sh
set +e
export PATH="$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin"
VBIN=/usr/local/hestia/bin
PAR=8
TMO=8

echo "===== 도메인별 헬스 점검 ====="

# 이 서버의 대표 IP — DNS 가 이 서버를 가리키는지 비교하는 기준
MYIP=$(ip route get 1.1.1.1 2>/dev/null | awk '{for(i=1;i<=NF;i++) if($i=="src") print $(i+1)}' | head -1)
[ -z "$MYIP" ] && MYIP=$(hostname -I 2>/dev/null | awk '{print $1}')
echo "  서버 IP ${MYIP:-?} · 동시 ${PAR} · 타임아웃 ${TMO}s"
echo "HM_DASH_MYIP=${MYIP:-}"
echo

# 검사 대상 수집: <계정> <도메인>
LIST=$(mktemp)
OUT=$(mktemp)
if [ -x "$VBIN/v-list-users" ]; then
  for U in $("$VBIN/v-list-users" plain 2>/dev/null | awk '{print $1}'); do
    "$VBIN/v-list-web-domains" "$U" plain 2>/dev/null | awk -v u="$U" '{print u" "$1}'
  done > "$LIST"
fi
NTOT=$(grep -c . "$LIST" 2>/dev/null); [ -z "$NTOT" ] && NTOT=0
if [ "$NTOT" = 0 ]; then
  echo "  검사할 도메인이 없습니다 (HestiaCP CLI 미검출 또는 도메인 0개)"
  echo "HM_DASH_DOMTOTAL=0"
  rm -f "$LIST" "$OUT"
  echo "===== 점검 끝 ====="
  exit 0
fi

# 도메인 1개 검사 — 병렬 실행되므로 한 줄로 결과를 출력한다
CHK=$(mktemp)
cat > "$CHK" <<'EOS'
#!/usr/bin/env bash
export PATH="$PATH:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin"
U="$1"; D="$2"; MYIP="$HM_MYIP"; TMO="$HM_TMO"
# 0) 웹루트 존재 — vhost 설정만 남고 파일이 없는 경우를 잡는다
ROOT=no; [ -d "/home/$U/web/$D/public_html" ] && ROOT=yes
# 1) 이 서버의 vhost 응답 (DNS 우회, 서버 자신에게 물어본다)
#    ※ Host 매칭이 실패하면 기본 vhost 가 200 을 주므로 이 값만으로 정상 판정하지 않는다.
LH=$(curl -sS -o /dev/null -m "$TMO" -k --resolve "$D:443:127.0.0.1" -w '%{http_code}' "https://$D/" 2>/dev/null)
[ -z "$LH" ] && LH=000
[ "$LH" = 000 ] && LH=$(curl -sS -o /dev/null -m "$TMO" --resolve "$D:80:127.0.0.1" -w '%{http_code}' "http://$D/" 2>/dev/null)
[ -z "$LH" ] && LH=000
# 2) 공개 경로 응답 (실제 방문자 관점 — DNS 를 따른다). 주 판정 지표.
PH=$(curl -sS -o /dev/null -m "$TMO" -k -w '%{http_code}' "https://$D/" 2>/dev/null)
[ -z "$PH" ] && PH=000
[ "$PH" = 000 ] && PH=$(curl -sS -o /dev/null -m "$TMO" -w '%{http_code}' "http://$D/" 2>/dev/null)
[ -z "$PH" ] && PH=000
# 3) DNS A 레코드가 이 서버인지 (IPv4 만 비교 — MYIP 가 IPv4 다)
A=$(dig +short +time=3 +tries=1 A "$D" 2>/dev/null | grep -E '^[0-9]+\.[0-9.]+$' | head -1)
[ -z "$A" ] && A=$(getent ahostsv4 "$D" 2>/dev/null | awk '{print $1}' | grep -E '^[0-9]+\.[0-9.]+$' | head -1)
if [ -z "$A" ]; then DNS=none; elif [ "$A" = "$MYIP" ]; then DNS=ok; else DNS=other; fi
# 4) 인증서 만료 D-day — 파일에서 읽는다(네트워크 불필요, 훨씬 빠르다)
CD=-
for C in "/home/$U/conf/web/$D/ssl/$D.crt" "/home/$U/conf/web/$D/ssl/$D.pem"; do
  [ -s "$C" ] || continue
  E=$(openssl x509 -enddate -noout -in "$C" 2>/dev/null | cut -d= -f2)
  [ -z "$E" ] && continue
  ES=$(date -d "$E" +%s 2>/dev/null) || continue
  CD=$(( (ES - $(date +%s)) / 86400 ))
  break
done
printf 'HM_DOMH %s %s %s %s %s %s %s %s\n' "$U" "$D" "$LH" "$PH" "$DNS" "$CD" "$ROOT" "${A:--}"
EOS
chmod +x "$CHK"

export HM_MYIP="$MYIP" HM_TMO="$TMO"
awk '{print $1" "$2}' "$LIST" | xargs -P "$PAR" -n 2 "$CHK" > "$OUT" 2>/dev/null

# 판정 규칙
#   DNS=ok   → 공개응답(PH)이 주 지표 (실제 방문자가 보는 것)
#   DNS=other/none → 그 자체가 이상. 이 경우 PH 는 남의 서버 응답이라 판정에 쓰지 않는다.
#   LH 단독으로는 정상 판정하지 않는다 — Host 매칭 실패 시 기본 vhost 가 200 을 준다.
issue_of() {   # $1=LH $2=PH $3=DNS $4=CD $5=ROOT $6=A  → 이상 사유(없으면 빈 문자열)
  local LH="$1" PH="$2" DNS="$3" CD="$4" ROOT="$5" A="$6" M=""
  [ "$ROOT" = no ] && M="웹루트 없음"
  if [ "$DNS" = none ]; then M="$M${M:+ · }DNS 레코드 없음"
  elif [ "$DNS" = other ]; then M="$M${M:+ · }DNS 가 다른 서버($A)"
  else
    case "$PH" in 2*|3*) ;; 000) M="$M${M:+ · }응답 없음(타임아웃/거부)" ;; *) M="$M${M:+ · }HTTP $PH" ;; esac
  fi
  if [ "$CD" != "-" ]; then
    if [ "$CD" -lt 0 ] 2>/dev/null; then M="$M${M:+ · }인증서 만료됨"
    elif [ "$CD" -le 14 ] 2>/dev/null; then M="$M${M:+ · }인증서 D-$CD"; fi
  fi
  printf '%s' "$M"
}

ISSUES=0; CERTSOON=0; CERTEXP=0; DNSBAD=0; HTTPBAD=0; NOROOT=0
while read -r _ U D LH PH DNS CD ROOT A; do
  [ -n "$(issue_of "$LH" "$PH" "$DNS" "$CD" "$ROOT" "$A")" ] && ISSUES=$((ISSUES+1))
  { [ "$DNS" = other ] || [ "$DNS" = none ]; } && DNSBAD=$((DNSBAD+1))
  [ "$ROOT" = no ] && NOROOT=$((NOROOT+1))
  if [ "$DNS" = ok ]; then
    case "$PH" in 2*|3*) ;; *) HTTPBAD=$((HTTPBAD+1)) ;; esac
  fi
  if [ "$CD" != "-" ]; then
    if [ "$CD" -lt 0 ] 2>/dev/null; then CERTEXP=$((CERTEXP+1))
    elif [ "$CD" -le 14 ] 2>/dev/null; then CERTSOON=$((CERTSOON+1)); fi
  fi
done < "$OUT"

echo "[이상 감지]"
while read -r _ U D LH PH DNS CD ROOT A; do
  M=$(issue_of "$LH" "$PH" "$DNS" "$CD" "$ROOT" "$A")
  [ -n "$M" ] && printf '  %-36s %s\n' "$D" "$M"
done < "$OUT"
[ "$ISSUES" = 0 ] && echo "  없음 — 전부 정상"
echo

echo "[원자료]"
cat "$OUT"
echo
echo "HM_DASH_DOMTOTAL=$NTOT"
echo "HM_DASH_DOMISSUES=$ISSUES"
echo "HM_DASH_DOMOK=$((NTOT - ISSUES))"
echo "HM_DASH_HTTPBAD=$HTTPBAD"
echo "HM_DASH_DNSBAD=$DNSBAD"
echo "HM_DASH_NOROOT=$NOROOT"
echo "HM_DASH_CERTSOON=$CERTSOON"
echo "HM_DASH_CERTEXP=$CERTEXP"
rm -f "$LIST" "$OUT" "$CHK"
echo "===== 점검 끝 ====="
```

### 5-2. 결과 저장

```rust
/// 도메인 1건의 헬스 (마커 HM_DOMH 한 줄)
#[derive(Default, Serialize, Deserialize, Clone)]
pub struct DomainHealth {
    #[serde(default)] pub account: String,
    #[serde(default)] pub domain: String,
    #[serde(default)] pub local_code: String,   // LH — 참고값. 단독 정상판정 금지
    #[serde(default)] pub public_code: String,  // PH — DNS=ok 일 때 주 지표
    #[serde(default)] pub dns: String,          // "ok"|"other"|"none"
    #[serde(default)] pub cert_days: String,    // "-" 또는 정수(음수=만료)
    #[serde(default)] pub webroot: String,      // "yes"|"no"
    #[serde(default)] pub a_record: String,
}
```

`Store` 에 `#[serde(default)] pub domain_health: Vec<DomainHealth>` +
`#[serde(default)] pub domain_health_at: i64` 추가.

파싱: `HM_DOMH ` 로 시작하는 줄을 공백으로 8필드 분해. 필드 수가 다르면 그 줄은 버린다.

### 5-3. UI

- 요약 배지: `이상 N건 / 전체 M건` — N>0 이면 빨강. 세부 카운트(HTTP/DNS/웹루트/인증서)도 함께
- 이상 목록만 표로 표시(정상은 접어둠). 컬럼: 도메인 · 사유 · DNS · 인증서 D-day
  사유 문자열은 **UI 에서 다시 만들지 말고** 스크립트의 `[이상 감지]` 섹션 출력을 그대로 로그에
  남기고, 표는 저장된 필드로 구성한다. 판정 기준이 두 곳에 생기면 어긋난다 —
  Rust 쪽 판정 함수는 스크립트 `issue_of()` 와 **같은 규칙**으로 한 곳에만 작성한다:

```rust
/// 스펙 §2-4 의 판정 규칙. 스크립트 issue_of() 와 동일하게 유지한다.
fn domain_issue(h: &DomainHealth) -> Option<String>
```

- 행 클릭 시 해당 계정의 계정 관리 페이지로 이동(선택 기능, 여유 있으면)
- 도메인이 많으면 오래 걸린다(동시 8개, 타임아웃 8초). 버튼에 예상 시간 힌트를 붙인다:
  "도메인 100개면 약 2분"

### DoD (Phase 3)

- [ ] `bash -n` 통과, 파괴 명령 부재 테스트
- [ ] `domain_issue()` 단위 테스트: DNS=other → 이상, ROOT=no → 이상,
      DNS=ok + PH=200 → 정상, DNS=ok + PH=500 → 이상, cert_days=-1 → 이상,
      cert_days=3 → 이상, cert_days=90 → 정상,
      **DNS=none + LH=200 → 이상**(기본 vhost 오탐 방지 회귀 테스트)
- [ ] 결과가 재시작 후에도 남는다

---

## 6. Phase 4 — 경고 집계와 바로가기

### 6-1. 통합 경고 배너

대시보드 최상단에 심각한 것만 모아 한 줄씩:

| 조건 | 문구 | 색 |
|---|---|---|
| `svc_fail` 비어있지 않음 | `서비스 정지: nginx` | 빨강 |
| `phpfpm_bad > 0` | `PHP-FPM 실패 N개` | 빨강 |
| `disk_max >= 95` | `디스크 95% (/)` | 빨강 |
| `cert_exp > 0` | `인증서 만료 N건` | 빨강 |
| `dom_issues > 0` | `도메인 이상 N건` | 주황 |
| `disk_max >= 90` | `디스크 90%` | 주황 |
| `diskmon == "1"` 이고 `diskmon_last` 가 36시간 초과 | `디스크 감시가 하루 넘게 안 돌았습니다` | 주황 |
| `diskmon == "0"` | `디스크 자동 감시 미설치` | 회색 |
| 아무것도 없음 | `이상 없음` | 초록 |

각 경고는 관련 화면으로 가는 버튼을 함께 둔다(예: PHP-FPM → 설정 > 연결 탭).

### 6-2. 바로가기 버튼

전체 사이트 · 일괄 업데이트(설정>일괄) · 디스크 점검(설정>디스크) · 설정.
기존 화면 전환 코드를 호출하기만 한다.

### DoD (Phase 4)

- [ ] 전체 `cargo test` 통과, 릴리스 빌드 경고 없음
- [ ] 경고 배너가 조건대로 뜨고 색이 맞다
- [ ] `docs/dashboard.md` 작성 — 2층 구조(왜 자동 SSH 를 안 하는지) · 각 지표의 의미 ·
      도메인 헬스 판정 규칙(§2-4)과 기본 vhost 오탐 함정 · 캐시 시각 읽는 법

---

## 7. 흔한 함정

1. **`format!` 안의 셸 중괄호** — `${VAR}`, `awk '{print}'` 가 깨진다.
   두 스크립트는 `const` 로 두고 `format!` 을 쓰지 말 것. 값 주입이 필요하면
   `replace("__HM_X__", &sq(v))` 패턴(계정 삭제 기능의 선례).
2. **자동 SSH 금지**(§2-1). "편의를 위해 대시보드 열 때 한 번만" 도 금지다.
   오프라인·서버다운 상태에서 앱이 멈춘 것처럼 보인다.
3. **`LH`(로컬 응답) 로 정상 판정하지 말 것**(§2-4). 실측에서 존재하지 않는 도메인이
   `200` 을 반환했다. 기본 vhost 가 응답하기 때문이다.
4. **집계와 목록의 기준을 하나로.** 이상 건수는 `domain_issue()` 가 `Some` 인 개수여야 한다.
   개별 카운터(HTTP/DNS/인증서)를 더해서 만들면 한 도메인이 여러 사유를 가질 때 중복 집계된다.
5. **디스크 최대 사용률은 실제 블록장치만.** 실측에서 `/sys/firmware/efi/efivars` 가
   섞여 들어왔다. 스크립트는 `case "$FS" in /dev/*)` 로 필터한다 — 지우지 말 것.
6. **파싱 실패에 관대하게.** 마커가 없거나 형식이 다르면 그 값만 버리고 화면은 계속 그린다.
   `unwrap()` 금지.
7. **스냅샷 커밋은 `Done{ok:true}` 에서만.** 중간에 실패하면 이전 캐시를 유지해야 한다
   (덮어써서 빈 값이 되면 "어제 정보" 조차 잃는다).

---

## 8. 최종 완료 기준

- [ ] Phase 1~4 각 DoD 통과, Phase 별 커밋 4개 이상
- [ ] `cargo test` 전부 통과 / `cargo build --release` 경고 없음
- [ ] 앱 시작 → 대시보드 즉시 표시, **SSH 0회**(로그에 아무 작업도 안 뜬다)
- [ ] 서버 헬스·도메인 헬스 각각 버튼으로만 실행되고, 결과가 재시작 후에도 남는다
- [ ] 두 스크립트에 파괴 명령이 없음(테스트로 강제)
- [ ] `domain_issue()` 회귀 테스트에 **DNS=none + LH=200 → 이상** 케이스 포함
- [ ] `docs/dashboard.md` 작성
