# 개발 스펙 — 대시보드 Phase 5 (수정·보강) · Codex 작업 지시서

작성 2026-07-30. **구현 담당: Codex.** Phase 1~4 구현물을 검증한 결과에서 나온 수정 항목이다.

- 대상 브랜치: `dashboard-impl` (PR #11)
- 원 스펙: [2026-07-30-dashboard-SPEC.md](2026-07-30-dashboard-SPEC.md)

---

## 0. 검증 결과 요약 — 무엇이 잘 됐는지

**Phase 1~4 는 스펙을 충실히 따랐다.** 아래는 재확인해 통과한 항목이므로 **건드리지 말 것**:

- 대시보드가 열릴 때 SSH 를 쏘지 않는다 (`dashboard_page` 에 `spawn` 호출 없음)
- `domain_issue()` 가 `local_code`(LH)를 아예 참조하지 않아 기본 vhost 200 오탐이 차단된다
- 회귀 테스트 `domain_health_issue_rules` 에 `DNS=none + LH=200 → 이상` 케이스가 있다
- 결과를 `Done{ok:true}` 에서만 커밋하고, 실패 시 이전 캐시를 유지한다
- 이상 건수를 `domain_issue()` 하나로 집계한다(개별 카운터 합산 아님)
- `need_update` 가 `status.starts_with("업데이트")` — 기존 색상 판정과 같은 기준
- 셸 2개가 스펙과 사실상 동일하고 실제 실행도 통과한다

이 문서의 항목만 고친다. 리팩터링·구조 변경은 하지 말 것.

---

## 1. [필수] 타임존 하드코딩 버그

### 증상

`parse_kst_datetime()` 이 마지막에 `- 9 * 3600` 으로 **KST(+9)를 고정 가정**한다.

```rust
Some(days * 86400 + hour * 3600 + min * 60 + sec - 9 * 3600)
```

이 함수는 디스크 감시의 `last-run` 파일(`date '+%F %T'` 로 기록된 **서버 로컬 시각**)을 파싱해
"36시간 넘게 안 돌았다" 를 판정하는 데 쓰인다(`app.rs:952`).

**서버 타임존이 KST 가 아니면 그 판정이 어긋난다.** 해외 VPS 는 UTC 가 기본인 경우가 많다.
UTC 서버라면 9시간 과거로 계산되어, 27시간만 지나도 "하루 넘게 안 돌았다" 경고가 뜬다.
반대로 UTC+14 지역이면 경고가 5시간 늦게 뜬다.

### 수정 방향 — 서버에서 epoch 로 변환해 넘긴다

문자열을 앱에서 해석하지 말고, **서버가 자기 타임존으로 epoch 를 계산해서 보내면** 타임존
문제가 사라진다.

**1) `SERVER_SNAPSHOT_BODY` 의 자동 감시 섹션 수정** — `HM_DASH_DISKMON_LAST_TS` 추가:

```sh
if [ -f /etc/cron.d/hm-disk-monitor ]; then
  DL=$(cat /var/lib/hm-disk-monitor/last-run 2>/dev/null)
  DR=$(grep -v '^date' /var/lib/hm-disk-monitor/history.tsv 2>/dev/null | tail -1 | awk '{print $2}')
  # 서버 타임존으로 epoch 변환 — 앱이 타임존을 추측하지 않게 한다
  DLTS=""
  [ -n "$DL" ] && DLTS=$(date -d "$DL" +%s 2>/dev/null)
  echo "  디스크 감시 설치됨 · 마지막 ${DL:-기록없음} · 최근판정 ${DR:-?}"
  echo "HM_DASH_DISKMON=1"
  echo "HM_DASH_DISKMON_LAST=${DL:-}"
  echo "HM_DASH_DISKMON_LAST_TS=${DLTS:-}"
  echo "HM_DASH_DISKMON_RESULT=${DR:-}"
else
  echo "  디스크 감시 미설치"
  echo "HM_DASH_DISKMON=0"
  echo "HM_DASH_DISKMON_LAST="
  echo "HM_DASH_DISKMON_LAST_TS="
  echo "HM_DASH_DISKMON_RESULT="
fi
```

기존 마커는 그대로 두고 **추가만** 한다(표시용 문자열은 계속 필요하다).

**2) `ServerSnapshot` 에 필드 추가**

```rust
/// 디스크 감시 마지막 실행 시각(unix초, 서버가 계산). 빈 문자열이면 미상.
#[serde(default)] pub diskmon_last_ts: String,
```

**3) 판정에서 epoch 를 우선 사용** (`app.rs:952` 부근)

```rust
// 서버가 준 epoch 를 우선 쓴다. 없을 때만(구버전 스냅샷 캐시) 문자열 파싱으로 폴백한다.
let last_ts = snapshot.diskmon_last_ts.trim().parse::<i64>().ok()
    .or_else(|| parse_server_datetime(&snapshot.diskmon_last));
if last_ts.is_some_and(|at| now_unix() - at > 36 * 3600) { /* 경고 */ }
```

**4) 함수 이름과 주석 정정**

`parse_kst_datetime` → **`parse_server_datetime`** 으로 이름을 바꾸고, 독타 주석에 한계를 적는다:

```rust
/// "YYYY-MM-DD HH:MM:SS" 또는 epoch 문자열을 unix초로. 날짜문자열은 KST(+9) 로 가정하므로
/// **폴백 전용**이다. 정확한 값은 서버가 계산한 `*_TS` 마커를 쓴다(스펙 Phase 5 §1).
```

기존 테스트 `disk_monitor_time_parsing` 의 함수명도 함께 바꾼다.

### 테스트 추가

```rust
#[test]
fn diskmon_last_prefers_server_epoch() {
    // epoch 가 있으면 그 값을 쓴다 (타임존 추측 없음)
    // epoch 가 없으면 문자열 폴백이 동작한다
    // 둘 다 없으면 None → 경고를 띄우지 않는다(모르는 것을 이상으로 단정하지 않는다)
}
```

세 경우를 각각 단정한다. 특히 **셋째**가 중요하다 — 값이 없을 때 경고를 띄우면
"감시가 죽었다" 오탐이 된다.

---

## 2. [권장] 즉시층 지표를 매 프레임 재계산하지 않기

`local_stats(&self.store)` 가 `dashboard_page()` 안에서 **매 프레임** 호출된다(`app.rs:894`).
`scan_cache` 전체를 순회하므로 사이트가 수천 개면 60fps × O(n) 이 된다.

수백 건에서는 체감되지 않으니 **급하지 않다.** 다만 대시보드가 기본 화면이라 항상 켜져 있다.

### 수정 방향

`App` 에 캐시 필드를 두고, 원본이 바뀔 때만 다시 계산한다.

```rust
/// 즉시층 지표 캐시. (scan_cache_at, scan_cache.len(), 활성 고객 수) 가 바뀌면 재계산한다.
dash_stats: Option<(i64, usize, usize, LocalStats)>,
```

`dashboard_page()` 진입부:

```rust
let key = (self.store.scan_cache_at, self.store.scan_cache.len(), active_customer_count);
let stats = match &self.dash_stats {
    Some((a, b, c, s)) if (*a, *b, *c) == key => s.clone(),
    _ => { let s = local_stats(&self.store); self.dash_stats = Some((key.0, key.1, key.2, s.clone())); s }
};
```

`LocalStats` 에 `#[derive(Clone)]` 이 필요하다. 도메인 추가/삭제는 활성 고객 수만으로는
안 잡히므로, 고객·도메인을 편집하는 경로에서 `self.dash_stats = None;` 을 한 줄 넣는다
(`dirty = true` 를 세우는 지점을 따라가면 된다).

**과하게 만들지 말 것.** 캐시 무효화가 복잡해지면 값이 틀리는 쪽이 더 나쁘다.
판단이 어려우면 이 항목은 건너뛰고 §1·§3만 해도 된다.

---

## 3. [권장] `docs/dashboard.md` 보강

현재 35줄로 스펙 DoD 의 4개 항목 중 "캐시 시각 읽는 법" 이 얇다. 아래를 추가한다.

1. **캐시 시각의 의미** — 대시보드 숫자는 "지금"이 아니라 **마지막 조회 시점** 기준이다.
   `3시간 전 기준` 이 무슨 뜻이고, 언제 새로고침해야 하는지.
   서버 헬스와 도메인 헬스는 **각각 따로** 조회되므로 두 시각이 다를 수 있다.
2. **왜 자동 조회를 안 하는지** (원 스펙 §2-1 요약) — 사용자가 "왜 자동으로 안 되냐"고
   물을 지점이다. 오프라인·서버다운에서 앱이 멈춰 보이는 것을 피하기 위함.
3. **타임존 주의** (§1 수정 후) — 디스크 감시 시각은 서버가 계산한 epoch 를 쓴다.
4. **도메인 헬스에 걸리는 시간** — 동시 8개·타임아웃 8초. 100개면 약 2분.

---

## 4. [선택] 이상 목록에서 계정 관리로 이동

원 스펙 §5-3 의 선택 기능. 이상 도메인 행을 클릭하면 해당 계정의 계정 관리 페이지를 연다.
기존 `open_account_page(ci)` 를 호출하면 된다. 계정명으로 `customers` 인덱스를 찾지 못하면
아무 동작도 하지 않는다(에러 표시하지 말 것 — 서버에만 있고 로컬에 등록 안 된 계정이 정상적으로 있다).

---

## 5. 작업 규칙

- **§1 은 필수**, §2·§3 은 권장, §4 는 선택. 우선순위대로 진행하고 커밋을 나눈다.
- §0 에 열거한 통과 항목의 동작을 바꾸지 않는다. 특히 `domain_issue()` 의 판정 규칙과
  "자동 SSH 금지" 는 이 기능의 존재 이유다.
- `cargo test` 전부 통과 / `cargo build --release` 경고 없음.
- 셸을 고쳤으면 `bash -n` 을 돌리고, 가능하면 실제 실행해 마커 출력을 확인한다.
- 주석·UI 문자열·커밋 메시지는 한국어.

### DoD

- [ ] `HM_DASH_DISKMON_LAST_TS` 가 스냅샷 스크립트의 **양쪽 분기**에서 출력된다
      (설치/미설치 모두 — 한쪽만 있으면 이전 값이 남는다)
- [ ] epoch 우선 → 문자열 폴백 → 없으면 경고 안 띄움, 세 경로 테스트 통과
- [ ] `parse_kst_datetime` 이름과 주석이 정정됐다
- [ ] `cargo test` 통과, 릴리스 빌드 경고 없음
- [ ] `docs/dashboard.md` 에 §3 의 4개 항목이 들어갔다
