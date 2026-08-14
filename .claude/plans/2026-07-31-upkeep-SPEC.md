# 개발 스펙 — 백업 현황·휴면 사이트 점검 (Codex 작업 지시서)

작성 2026-07-31. **구현 담당: Codex.** 판단이 끝난 확정 스펙이다.

- 대상 브랜치: `dashboard-impl`(PR #11) 위에 이어서 작업
- 선행: [dashboard-SPEC.md](2026-07-30-dashboard-SPEC.md) · [PHASE5](2026-07-30-dashboard-PHASE5.md) · [PHASE6](2026-07-30-dashboard-PHASE6.md)

---

## 0. 배경

운영 중 나온 요청 2가지다.

1. **백업이 실제로 돌고 있는지 알 수 없다.** HestiaCP 자동 백업은 cron 으로 돌고 세대 수만큼
   자동 정리되지만, **특정 계정만 조용히 실패**해도 아무도 모른다. 정작 필요할 때 없으면 끝이다.
2. **오래 방치된 사이트를 찾고 싶다.** 디스크만 차지하고 아무도 안 쓰는 사이트가 쌓인다.

디스크 감시와 같은 문제 구조다 — **조용히 실패하고, 안 보면 모른다.**

---

## 1. 작업 규칙

- 이 문서에 없는 판단은 하지 말고 질문한다. **Phase 7-A → 7-B 순서로 커밋을 나눈다.**
- 빌드 `cargo build` / 테스트 `cargo test` / 릴리스 `cargo build --release`
- 주석·UI 문자열·커밋 메시지는 한국어.

### 절대 규칙 (선행 스펙에서 이어짐)

1. **대시보드가 자동으로 SSH 를 쏘지 않는다.** 이 점검도 **버튼을 눌렀을 때만** 실행한다.
2. **읽기 전용이다.** 이 스크립트는 어떤 파일도 지우거나 고치지 않는다.
   `rm`·`v-delete-`·`DROP` 이 들어가면 안 되고, 테스트로 강제한다.
3. **휴면 후보는 "삭제 대상"이 아니라 "확인 대상"이다.** §3-2 를 반드시 지킬 것.
4. 자격증명을 소스·테스트에 넣지 않는다.

---

## 2. Phase 7-A — 백업 현황

### 2-1. `ops.rs` 신규

```rust
/// 백업 현황 + 휴면 사이트 점검 (SSH, sudo, 읽기 전용).
/// 백업 누락 계정·마지막 백업 시각과, 오래 방치된 사이트 후보를 한 번에 조사한다.
pub fn build_upkeep_check(s: &Settings) -> Result<Job, String>
```

SSH 1회로 둘 다 수집한다(§3 과 같은 스크립트). 조회를 나누지 말 것.

### 2-2. 수집 항목

| 마커 | 뜻 |
|---|---|
| `HM_BK_CRON` | 자동 백업 cron 등록 여부(`1`/`0`) |
| `HM_BK_KEEP` | 보관 세대 수(`BACKUPS` 설정값) |
| `HM_BK_TOTAL` | `/backup` 의 `.tar` 개수 |
| `HM_BK_SIZE` | `/backup` 총 용량 |
| `HM_BK_USERS` | HestiaCP 계정 수 |
| `HM_BK_MISSING` | **백업이 하나도 없는 계정 목록**(공백 구분) |
| `HM_BK_NEWEST` | 가장 최근 백업 시각(epoch) |
| `HM_BK_STALEST` | `<계정> <일수>` — 최신 백업이 가장 오래된 계정 |
| `HM_BKU <계정> <세대수> <며칠전>` | 계정별 한 줄 |

**핵심은 `HM_BK_MISSING` 과 `HM_BKU` 다.** 전체가 잘 도는 것처럼 보여도 특정 계정만
빠져 있는 경우를 잡는다.

---

## 3. Phase 7-B — 휴면 사이트 점검

### 3-1. 판정 기준

두 신호를 보고 **2개 이상**일 때만 후보로 올린다.

| 신호 | 판정 | 근거 |
|---|---|---|
| 접근 로그 mtime 이 90일 이상 전이거나 로그가 아예 없음 | +1 | 방문자가 없다 = 쓰이지 않는다 |
| 웹루트에 180일 내 수정된 파일이 없음 | +1 | 관리자도 손대지 않는다 |

- 캐시·로그·업로드 디렉터리는 자동 갱신되므로 **파일 수정 검사에서 제외**한다.
- 로그가 로테이션되면 mtime 이 최근으로 보인다 → **"살아있다"로 판정**된다.
  이 방향의 오류는 **의도한 것**이다(§3-2).

### 3-2. 오판 방향을 한쪽으로 고정한다

**살아있는 사이트를 "휴면"이라고 하면 안 된다. 놓치는 건 괜찮다.**

- 계절성 사이트(연 1회 행사), 내부용 사이트, 봇만 오는 사이트가 섞일 수 있다
- 그래서 신호 1개로는 후보에 올리지 않는다
- UI 문구는 **"정리 후보"가 아니라 "확인 권장"** 이다. 버튼 이름에 `삭제`를 쓰지 말 것
- 계정/도메인 삭제 기능(PR #8)과 **자동으로 연결하지 않는다.**
  목록에서 바로 삭제로 넘어가는 동선을 만들지 말 것 — 한 번 더 사람이 판단해야 한다

### 3-3. 수집 항목

| 마커 | 뜻 |
|---|---|
| `HM_IDLE <계정> <도메인> <접근없는일수> <파일오래됨0/1> <신호수>` | 후보 한 줄 (`접근없는일수 = -1` 은 로그 자체가 없음) |
| `HM_IDLE_COUNT` | 후보 개수 |

용량은 **이미 `scan_cache` 에 있다**(`file_bytes`/`db_bytes`). 서버에서 다시 재지 말고
UI 에서 도메인명으로 조인해 함께 보여준다.

---

## 4. 스크립트 (실행 검증 완료 — 그대로 쓸 것)

```sh
echo "[백업 현황]"
BCRON=0
grep -rqi 'backup' /etc/cron.d/hestia 2>/dev/null && BCRON=1
BKEEP=$(grep -E "^BACKUPS=" /usr/local/hestia/conf/hestia.conf 2>/dev/null | cut -d"'" -f2)
[ -z "$BKEEP" ] && BKEEP="-"
echo "  자동 백업 cron: $([ "$BCRON" = 1 ] && echo 등록됨 || echo '없음(수동 백업만)')   보관 세대: $BKEEP"
echo "HM_BK_CRON=$BCRON"
echo "HM_BK_KEEP=$BKEEP"

if [ ! -d "$BDIR" ]; then
  echo "  ✗ $BDIR 없음"
  echo "HM_BK_TOTAL=0"; echo "HM_BK_USERS=0"; echo "HM_BK_MISSING="
else
  NTAR=$(ls -1 "$BDIR"/*.tar 2>/dev/null | grep -c .)
  SIZE=$(du -sh "$BDIR" 2>/dev/null | cut -f1)
  echo "  파일 ${NTAR}개 · ${SIZE:-?}"
  echo "HM_BK_TOTAL=$NTAR"
  echo "HM_BK_SIZE=${SIZE:-}"

  USERS=$("$VBIN/v-list-users" plain 2>/dev/null | awk '{print $1}')
  NU=$(printf '%s\n' "$USERS" | grep -c .)
  MISSING=""; NEWEST=0; OLDESTU=""; OLDESTT=0
  for U in $USERS; do
    LAST=$(ls -1t "$BDIR/$U".*.tar 2>/dev/null | head -1)
    if [ -z "$LAST" ]; then MISSING="$MISSING $U"; continue; fi
    N=$(ls -1 "$BDIR/$U".*.tar 2>/dev/null | grep -c .)
    T=$(stat -c %Y "$LAST" 2>/dev/null); [ -z "$T" ] && T=0
    [ "$T" -gt "$NEWEST" ] 2>/dev/null && NEWEST=$T
    # 가장 오래된 '최신 백업' = 제일 방치된 계정
    if [ "$OLDESTT" = 0 ] || { [ "$T" -lt "$OLDESTT" ] 2>/dev/null; }; then OLDESTT=$T; OLDESTU="$U"; fi
    AGE=$(( (NOW - T) / 86400 ))
    printf 'HM_BKU %s %s %s\n' "$U" "$N" "$AGE"
  done
  NMISS=$(printf '%s' "$MISSING" | wc -w)
  echo "  계정 ${NU}개 중 백업 있음 $((NU - NMISS))개 · 없음 ${NMISS}개"
  [ -n "$MISSING" ] && echo "    백업 없는 계정:$MISSING"
  if [ "$NEWEST" -gt 0 ] 2>/dev/null; then
    echo "  가장 최근 백업: $(date -d "@$NEWEST" '+%F %T') ($(( (NOW - NEWEST) / 86400 ))일 전)"
  else
    echo "  ✗ 백업 파일이 하나도 없습니다"
  fi
  echo "HM_BK_USERS=$NU"
  echo "HM_BK_MISSING=$(printf '%s' "$MISSING" | sed 's/^ *//')"
  echo "HM_BK_NEWEST=$NEWEST"
  [ -n "$OLDESTU" ] && echo "HM_BK_STALEST=$OLDESTU $(( (NOW - OLDESTT) / 86400 ))"
fi
echo

echo "[휴면 후보 — 오래 방치된 사이트]"
echo "  ※ 삭제 대상이 아니라 '확인해볼 곳' 이다. 계절성·내부용 사이트가 섞일 수 있다."
HITDAYS=90     # 접근 로그가 이만큼 없으면 신호
MODDAYS=180    # 파일이 이만큼 안 바뀌면 신호
NCAND=0
shopt -s nullglob
for WR in /home/*/web/*/public_html; do
  [ -d "$WR" ] || continue
  DOM="$(basename "$(dirname "$WR")")"
  OWN="$(stat -c %U "$WR" 2>/dev/null)"; [ -z "$OWN" ] && continue

  # 1) 마지막 접근 — 로그 파일의 mtime. 로테이션되면 최근으로 보이므로 '살아있다' 쪽으로 안전하게 기운다.
  HITT=0
  for L in /var/log/apache2/domains/"$DOM".log /var/log/nginx/domains/"$DOM".log \
           /home/"$OWN"/web/"$DOM"/logs/"$DOM".log; do
    [ -s "$L" ] || continue
    T=$(stat -c %Y "$L" 2>/dev/null); [ -z "$T" ] && continue
    [ "$T" -gt "$HITT" ] 2>/dev/null && HITT=$T
  done
  if [ "$HITT" = 0 ]; then HITD=-1; else HITD=$(( (NOW - HITT) / 86400 )); fi

  # 2) 마지막 파일 수정 — 캐시·로그는 자동 갱신되므로 제외한다.
  #    -mtime 은 POSIX 라 어떤 find 에서도 동작한다(-newermt 는 상대시간 표기를 거부하는 구현이 있다).
  #    제외 경로는 반드시 "$WR" 기준으로 쓴다. '*/tmp/*' 처럼 쓰면 상위 경로에 tmp 가 있을 때 전부 걸러진다.
  FRESH=$(find "$WR" -type f -mtime -"$MODDAYS" \
            -not -path "$WR/*cache/*" -not -path "$WR/logs/*" -not -path "$WR/tmp/*" \
            -not -path "$WR/wp-content/uploads/*" -print -quit 2>/dev/null)
  [ -n "$FRESH" ] && MODOLD=0 || MODOLD=1

  # 3) 신호 집계 — 2개 이상이면 후보
  SIG=0; WHY=""
  if [ "$HITD" = -1 ]; then SIG=$((SIG+1)); WHY="접근로그 없음"
  elif [ "$HITD" -ge "$HITDAYS" ] 2>/dev/null; then SIG=$((SIG+1)); WHY="${HITD}일간 접근 없음"; fi
  if [ "$MODOLD" = 1 ]; then SIG=$((SIG+1)); WHY="$WHY${WHY:+ · }${MODDAYS}일+ 파일 변경 없음"; fi

  if [ "$SIG" -ge 2 ] 2>/dev/null; then
    NCAND=$((NCAND+1))
    printf '  %-34s %-10s %s\n' "$DOM" "$OWN" "$WHY"
    printf 'HM_IDLE %s %s %s %s %s\n' "$OWN" "$DOM" "$HITD" "$MODOLD" "$SIG"
  fi
done
[ "$NCAND" = 0 ] && echo "  없음 — 최근 활동이 확인되지 않는 사이트가 없습니다"
echo "HM_IDLE_COUNT=$NCAND"
```

### 검증한 동작

가짜 환경(계정 3개·사이트 4개)을 만들어 실제로 돌린 결과다. **아래 도메인은 전부 테스트용
가짜 데이터이고 실제 서버와 무관하다.**

| 대상 | 상태 | 결과 |
|---|---|---|
| 계정 `eond` | 백업 3세대, 1일 전 | `HM_BKU eond 3 1` |
| 계정 `rokmc` | 백업 1세대, 40일 전 | `HM_BKU rokmc 1 40` · `HM_BK_STALEST=rokmc 40` |
| 계정 `ghost` | 백업 없음 | `HM_BK_MISSING=ghost` |
| 사이트 (활성) | 로그 최신 + 파일 최신 | 후보 아님 |
| 사이트 (200일 접근 없음 + 파일 300일) | 신호 2 | **후보** |
| 사이트 (로그 없음 + 파일 400일) | 신호 2 | **후보** |
| 사이트 (로그 없음 + **파일 최신**) | 신호 1 | **후보 아님** ← 오판 방지 확인 |

마지막 줄이 중요하다. 로그가 없어도 파일이 최근에 바뀌었으면 누군가 관리 중인 사이트다.

### 검증 중 잡은 결함 2건 (수정 완료 — 되돌리지 말 것)

1. **`-newermt "-180 days"` 는 이식성이 없다.** 일부 `find` 구현(bfs 등)이 상대시간 표기를
   거부한다. **POSIX `-mtime -180`** 으로 바꿨다.
2. **`-not -path '*/tmp/*'` 는 상위 경로까지 걸러낸다.** 웹루트 위 어딘가에 `tmp` 가 있으면
   모든 파일이 제외되어 **살아있는 사이트가 휴면으로 오판**됐다.
   제외 경로를 **`"$WR"` 기준 절대경로**로 고쳤다.

---

## 5. UI

대시보드에 카드 2개를 추가한다. 조회 버튼은 **하나**다(`백업·정리 점검`, SSH 1회).

### 5-1. 백업 현황 카드

```
백업 현황                                        [점검]
  자동 백업 cron 등록됨 · 보관 3세대 · 파일 84개 · 215G
  계정 28개 중 백업 있음 26 · 없음 2          ← 없음이 0 이 아니면 빨강
    백업 없는 계정: ghost, testuser
  가장 최근 백업 1일 전 · 가장 밀린 계정 rokmc (40일)
```

- `HM_BK_CRON=0` 이면 빨강 `자동 백업이 등록되어 있지 않습니다`
- `HM_BK_MISSING` 이 비어있지 않으면 빨강, 계정 목록을 그대로 보여준다
- `HM_BK_NEWEST` 가 **3일 이상 전**이면 주황 `백업이 N일째 갱신되지 않았습니다`
- 계정별 표(`HM_BKU`)는 접어두고, 펼치면 세대 수·경과일 정렬

### 5-2. 휴면 사이트 카드

```
확인 권장 사이트                                  3건
  ※ 삭제 대상이 아니라 확인해볼 곳입니다. 계절성·내부용 사이트가 섞일 수 있습니다.
  도메인            계정     사유                          용량
  a.example.com     rokmc    200일간 접근 없음 · 파일 변경 없음   1.2GB
```

- 용량은 `scan_cache` 에서 조인(없으면 `-`)
- 도메인 클릭 → 계정 관리 페이지(기존 `open_account_page`). **삭제 화면으로 바로 보내지 말 것**
- 0건이면 초록 `최근 활동이 확인되지 않는 사이트가 없습니다`
- 목록은 §PHASE6 4-1 과 같은 방식으로 **스크롤 + TSV 복사** 를 붙인다

### 5-3. 저장

```rust
#[serde(default)] pub backup_status: BackupStatus,   // 위 HM_BK_* 를 담는 구조체
#[serde(default)] pub backup_users: Vec<BackupUser>, // HM_BKU
#[serde(default)] pub idle_sites: Vec<IdleSite>,     // HM_IDLE
#[serde(default)] pub upkeep_at: i64,
```

`Done{ok:true}` 에서만 커밋하고, 실패 시 이전 값을 유지한다(선행 스펙 함정 7과 동일).

---

## 6. 함정

1. **자동 SSH 금지** — 버튼을 눌렀을 때만.
2. **읽기 전용 강제** — 스크립트에 `rm`·`v-delete-`·`DROP` 이 없어야 하고 테스트로 확인한다.
3. **`-mtime` 을 `-newermt` 로 되돌리지 말 것**(§4 결함 1).
4. **제외 경로를 `"$WR"` 기준으로 유지할 것**(§4 결함 2). `*/tmp/*` 같은 전역 패턴 금지.
5. **신호 1개짜리를 후보에 넣지 말 것.** 오판이 늘고 그러면 목록 전체를 안 믿게 된다.
6. 휴면 목록에서 **삭제 기능으로 가는 동선을 만들지 말 것**(§3-2).
7. 계정명이 다른 계정의 접두어인 경우(`eond` / `eond2`)가 있다. 백업 파일 glob 은
   `"$U".*.tar` 처럼 **점을 포함**해야 한다. `"$U"*.tar` 로 바꾸면 서로 섞인다.
8. `find` 가 97개 사이트를 도는 데 시간이 걸린다. `-print -quit` 로 **첫 매치에서 멈추는**
   현재 구조를 유지할 것(전체 순회로 바꾸면 몇 분씩 걸린다).

---

## 7. DoD

- [ ] `cargo test` 전부 통과 / `cargo build --release` 경고 없음
- [ ] 스크립트에 파괴 명령이 없음(테스트로 강제)
- [ ] `bash -n` 통과
- [ ] 백업 없는 계정이 있으면 빨강으로 계정명이 표시된다
- [ ] `HM_BK_CRON=0` 일 때 경고가 뜬다
- [ ] 신호 1개짜리 사이트가 후보에 **오르지 않는다**(단위 테스트)
- [ ] 휴면 목록에서 삭제 화면으로 가는 버튼이 **없다**
- [ ] 휴면 목록이 스크롤되고 TSV 복사가 된다
- [ ] `docs/dashboard.md` 에 백업 점검·휴면 판정 기준과 **오판 방향 고정 원칙**(§3-2)을 추가
