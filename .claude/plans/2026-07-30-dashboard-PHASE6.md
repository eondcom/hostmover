# 개발 스펙 — 대시보드 Phase 6 (상태 등급·추이 점검·목록 UI) · Codex 작업 지시서

작성 2026-07-30. **구현 담당: Codex.** 판단이 끝난 확정 스펙이다.

- 대상 브랜치: `dashboard-impl` (PR #11) 위에 이어서 작업
- 선행 문서: [dashboard-SPEC.md](2026-07-30-dashboard-SPEC.md) · [dashboard-PHASE5.md](2026-07-30-dashboard-PHASE5.md)

---

## 0. 배경 — 사용자가 요청한 것

실제 운영 중 나온 요청 3가지다.

1. **숫자만 보고는 좋은지 나쁜지 모른다.** 등급(A~E, 우수~위험)과 함께 무엇을 해야 하는지 안내.
2. **"디스크 추이를 봐라", "`/backup` 파티션을 확인해라" 같은 조언을 앱이 직접 점검**해서 알려주기.
3. **도메인 헬스 목록이 스크롤도 안 되고 복사도 안 된다.** 97개 도메인에서 쓸 수 없다.
4. **디스크가 여러 개다(운영·백업 분리).** 지금은 최대 사용률 하나만 보여준다.
   **디스크마다 따로** 헬스를 검사해 보여줘야 한다.

   실제 대상 서버(mars.eond.com)의 구성 — 물리 디스크 3개:

   ```
   /dev/nvme1n1p2  468G   14%  /         ← OS
   /dev/nvme0n1p1  469G   69%  /home     ← 운영 데이터
   /dev/sda1       3.6T    7%  /backup   ← 백업 (별도 물리 디스크)
   ```

   `/` 와 `/home` 이 **서로 다른 NVMe** 이고 `/backup` 은 별도 SATA 다.
   최대값 하나(69%)만 보여주면 이 구조가 통째로 사라진다.
5. **대시보드 화면 자체가 스크롤되지 않는다.** `dashboard_page` 는 `CentralPanel` 안에
   카드를 바로 쌓는데 `ScrollArea` 가 **하나도 없어서**, 카드가 화면을 넘으면 잘린다.
   이번에 등급 카드와 디스크 목록이 추가되면 더 심해진다.

---

## 1. 작업 규칙

- 이 문서에 없는 판단은 하지 말고 질문한다.
- **Phase 6-A → 6-B → 6-C 순서로, 각각 커밋을 나눈다.**
- 빌드 `cargo build` / 테스트 `cargo test` / 릴리스 `cargo build --release`
- 주석·UI 문자열·커밋 메시지는 한국어.

### 절대 규칙 (선행 스펙에서 이어짐)

1. **대시보드가 자동으로 SSH 를 쏘지 않는다.** 등급 계산은 **이미 받아온 캐시로만** 한다.
   등급을 매기려고 새 조회를 트리거하지 말 것.
2. **SSH 는 여전히 스냅샷 1회.** Phase 6-B 의 새 점검은 `SERVER_SNAPSHOT_BODY` **안에 추가**한다.
   별도 `build_*` 함수를 만들어 조회를 2회로 늘리지 말 것.
3. `domain_issue()` 판정 규칙(LH 단독 정상판정 금지)을 바꾸지 않는다.
4. 자격증명을 소스·테스트에 넣지 않는다.

---

## 2. Phase 6-A — 상태 등급 평가

### 2-1. 등급 체계

`A 우수` · `B 양호` · `C 주의` · `D 경고` · `E 위험` 5단계. 표시는 `A (우수)` 형태.

```rust
/// 서버 상태 등급. 낮을수록(A) 좋다.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Grade { A, B, C, D, E }

impl Grade {
    fn label(self) -> &'static str {
        match self { Grade::A=>"우수", Grade::B=>"양호", Grade::C=>"주의", Grade::D=>"경고", Grade::E=>"위험" }
    }
    fn letter(self) -> &'static str {
        match self { Grade::A=>"A", Grade::B=>"B", Grade::C=>"C", Grade::D=>"D", Grade::E=>"E" }
    }
    fn color(self) -> egui::Color32 {
        match self {
            Grade::A => C_GREEN,
            Grade::B => egui::Color32::from_rgb(0x6E, 0xA8, 0x4F),
            Grade::C => egui::Color32::from_rgb(0xDC, 0x96, 0x3C),
            Grade::D => egui::Color32::from_rgb(0xD1, 0x6B, 0x3E),
            Grade::E => C_RED,
        }
    }
}
```

### 2-2. 항목별 기준 (그대로 구현할 것)

| 항목 | A | B | C | D | E | 근거 |
|---|---|---|---|---|---|---|
| **부하** `load1/cores` | <0.5 | <0.8 | <1.2 | <2.0 | ≥2.0 | 1.0 = 코어 포화 시작점. 2.0 이상은 대기가 쌓이는 상태 |
| **메모리** | <60% | <75% | <85% | <93% | ≥93% | 93% 넘으면 스왑·OOM 위험 |
| **디스크** (디스크별, §2-2-1) | <70% | <80% | <88% | <95% | ≥95% | 95%는 서비스 장애 임박. 88%는 대응 시간 확보선 |
| **서비스 정지** | 0개 | — | — | — | 1개 이상 | 하나라도 죽으면 그 자체로 장애다 |
| **PHP-FPM 실패** | 0개 | — | — | 1개 | 2개 이상 | 실패 1개 = 일부 사이트 다운 |
| **도메인 이상** 비율 | 0% | ≤2% | ≤5% | ≤15% | >15% | 테스트 도메인 등 1~2건은 흔한 잡음. 15% 초과는 구조적 문제 |
| **인증서** | 만료0·임박0 | — | 임박≥1 | 만료1 | 만료≥2 | §2-2-2 참고 |
| **자동 감시** | 설치+최근 | 설치+지연(36h+) | 미설치 | — | — | 서버 건강이 아니라 운영 준비 상태 |

### 2-2-2. 인증서 — 이 서버는 **자동 갱신이 돌고 있다**

Let's Encrypt 는 90일 유효이고 HestiaCP 가 만료 30일쯤 전에 자동 갱신한다. 따라서 정상이라면
D-day 는 **30~90 사이를 오간다.**

**D-14 이하로 내려왔다면 그건 "곧 만료"가 아니라 "자동 갱신이 이미 몇 번 실패했다"는 뜻이다.**
그래서 임박 1건만 있어도 **C(주의)** 로 잡고, 문구도 다르게 쓴다:

| 상태 | 문구 |
|---|---|
| 만료됨 | `인증서 만료 — 접속이 차단됩니다` |
| D-14 이하 | `인증서 D-N — 자동 갱신이 실패하고 있을 수 있습니다` |

"곧 만료됩니다" 라고만 쓰면 사용자가 "자동 갱신되니 괜찮겠지" 하고 넘긴다. 실제로는
갱신이 깨진 상태이므로 **원인을 확인해야 한다**는 것이 전달되어야 한다.
(흔한 원인: DNS 가 다른 서버를 가리켜 `.well-known` 검증 실패 — 도메인 헬스의 `DNS=other` 와
같이 뜨면 거의 확실하다)

### 2-2-1. 디스크는 **하나로 뭉치지 않는다**

운영 디스크와 백업 디스크는 성격이 다르다. 운영이 60%인데 백업이 95%면 "디스크 95%" 하나로는
어느 쪽이 위험한지 알 수 없고, **백업 디스크가 차면 백업이 실패**한다는 점이 드러나지 않는다.

**디스크마다 등급을 매기고, 디스크 항목의 등급은 그중 최악을 쓴다.**

한 디스크의 등급은 아래 중 **최악**이다:

| 세부 | A | B | C | D | E |
|---|---|---|---|---|---|
| 사용률 | <70% | <80% | <88% | <95% | >=95% |
| inode 사용률 | <70% | <80% | <88% | <95% | >=95% |
| SMART 상태 | `PASSED` | - | 조회불가(`-`) | - | 그 외(`FAILED` 등) |
| 재할당+대기 섹터 | 둘 다 0 | - | 합계 1~9 | 합계 10~49 | 합계 50+ |
| FS 누적 에러 | 0 | - | 1~9 | 10+ | - |

- SMART 조회불가(`-`)는 **C(주의)** 로 둔다. A 로 두면 smartmontools 미설치 서버가 영원히
  "우수"로 보이고, E 로 두면 조회가 원래 안 되는 환경에서 계속 빨간불이라 무시하게 된다.
- 값이 `-` 인 세부는 그 세부만 건너뛴다(디스크 전체를 미조회로 만들지 않는다).

**백업 디스크 가중**: 역할이 `backup` 인 디스크가 **D 이하**면 권고에 반드시 넣는다(§3-3).
백업이 실패하면 계정 삭제·복구 경로가 통째로 막힌다.

### 2-3. 총합 등급 = **최악 항목** (평균 금지)

평균을 내면 서비스가 죽었는데 B 가 나온다. 그건 거짓 안심이고 이 기능의 목적을 정면으로 배반한다.

```rust
/// 총합 등급 = 항목 중 최악. 단 '자동 감시' 는 총합을 C 보다 나쁘게 만들지 않는다
/// (운영 준비 상태이지 서버 건강 문제가 아니다).
fn overall_grade(items: &[(String, Grade, bool /*is_monitoring*/)]) -> Option<Grade>
```

- 항목이 하나도 없으면 `None` → 화면에 **"미조회"** 로 표시한다.
- **모르는 것을 A 로 표시하지 말 것.** 서버 헬스를 조회하지 않았는데 A 를 보여주면 최악의 오해다.
- `자동 감시` 항목은 `min(grade, C)` 로 클램프해서 총합에 반영한다.

### 2-4. 조회하지 않은 항목은 제외한다

- 서버 스냅샷 미조회(`snapshot.at <= 0`) → 부하·메모리·디스크·서비스·PHP-FPM·감시 항목 **전부 제외**
- 도메인 헬스 미점검(`domain_health.is_empty()`) → 도메인·인증서 항목 **제외**하고
  등급 옆에 `도메인 미점검` 을 회색으로 병기

**도메인을 점검하지 않은 상태에서 A 를 주면 안 된다.** 사용자가 실제로 겪은 상황이다
(서버 리소스는 멀쩡한데 개별 사이트가 죽어 있는 경우).

### 2-5. UI

대시보드 최상단, 기존 경고 배너 **위**에 등급 카드를 놓는다.

```
┌ 종합 상태 ─────────────────────────────────────────────┐
│  ┌───┐                                                  │
│  │ B │  양호    mars.eond.com · 방금 기준                │
│  └───┘          도메인 미점검                            │
│                                                          │
│  부하 A · 메모리 A · 디스크 C · 서비스 A · PHP-FPM A     │
│  감시 B                                                  │
└──────────────────────────────────────────────────────────┘
```

- 등급 글자는 크게(`RichText::new("B").size(32.0).strong()`), 색은 `Grade::color()`
- 항목별 등급을 한 줄로 나열. C 이하인 항목은 색을 입혀 눈에 띄게
- 각 항목에 `on_hover_text` 로 실제 수치와 기준을 보여준다
  (예: `"디스크 69% · C 주의 (기준: A<70 B<80 C<88 D<95 E≥95)"`)
- 미조회 상태: 등급 자리에 `?` 회색 + `서버 헬스를 조회하면 등급이 표시됩니다`

### 2-5-1. 디스크 목록 표시

등급 카드 아래(또는 서버 헬스 카드 안)에 **디스크마다 한 줄**로 보여준다.

```
디스크
  ● /home    운영   82%  170G 남음  inode 9%  SMART PASSED           +0.21%p/일 · 95%까지 61일   B
  ● /backup  백업   94%  110G 남음  inode 1%  SMART FAILED! 섹터 60  +0.43%p/일 · 95%까지 2일    E
```

- **추이는 디스크마다 따로** 보여준다. 위 예에서 `/home` 은 두 달 여유인데 `/backup` 은 이틀 뒤
  꽉 찬다 — 이 차이가 이 기능의 핵심이다
- `rate` 가 `-` 면 `추이 데이터 쌓이는 중` (감시 설치 후 2일 필요)
- `eta` 가 `-` 면 증가하지 않는 것이므로 예상일을 적지 않는다(감소 중일 수도 있다)

- 앞의 `●` 를 디스크 등급 색으로 칠한다
- 역할은 `운영`/`백업`/`기타` 로 한글 표시
- `-` 인 값은 칸을 비우지 말고 `조회불가` 로 적는다(빈칸은 "정상"으로 오해된다)
- 디스크가 1개뿐이어도 같은 형식으로 보여준다(형식이 바뀌면 혼란)
- 마우스를 올리면 물리 디스크명·가동시간·온도를 `on_hover_text` 로

### 2-6. 테스트

```rust
#[test]
fn grade_rules_and_overall() {
    // 항목별 경계값: 0.79/0.80, 74/75, 87/88 등 각 경계에서 등급이 바뀌는지
    // 총합은 최악 항목을 따른다 (평균 아님)
    // 서비스 정지 1개 → 총합 E
    // 감시 미설치(C)만 나쁠 때 → 총합이 C 보다 나빠지지 않는다
    // 항목이 없으면 None (미조회)
    // 도메인 미점검이면 도메인·인증서 항목이 제외된다
}

#[test]
fn disk_grade_rules() {
    // 사용률 94% → D, 95% → E (경계)
    // SMART "PASSED" → A, "FAILED!" → E, "-" → C (미조회를 A 로 두지 않는다)
    // 재할당+대기 합계 9 → C, 10 → D, 50 → E
    // 한 디스크의 등급 = 세부 중 최악
    // 디스크 항목 등급 = 디스크들 중 최악 (운영 A + 백업 E → E)
    // 세부값이 "-" 면 그 세부만 건너뛰고 나머지로 판정한다
}
```

---

## 3. Phase 6-B — 추이·구성 점검과 권고

### 3-1. `SERVER_SNAPSHOT_BODY` 에 아래 섹션을 **추가**한다

**이 스크립트는 실제 실행 검증했다(추이 계산·경계 케이스 포함). 그대로 쓸 것.**
`HIST=/var/lib/hm-disk-monitor/history.tsv` 를 스크립트 앞부분 변수 선언에 추가한다.

```sh
echo "[추이·구성 점검]"

# 1) 디스크 사용률 추이 — 지금 값보다 '오르는 속도'가 중요하다
RATE=""; ETA=""; SPAN=""
if [ -s "$HIST" ]; then
  ROWS=$(grep -v '^date' "$HIST" 2>/dev/null | awk -F'\t' 'NF>=6' | tail -30)
  N=$(printf '%s\n' "$ROWS" | grep -c .)
  if [ "${N:-0}" -ge 2 ] 2>/dev/null; then
    F=$(printf '%s\n' "$ROWS" | head -1); L=$(printf '%s\n' "$ROWS" | tail -1)
    D1=$(printf '%s' "$F" | cut -f1); U1=$(printf '%s' "$F" | cut -f6 | tr -d '%')
    D2=$(printf '%s' "$L" | cut -f1); U2=$(printf '%s' "$L" | cut -f6 | tr -d '%')
    S1=$(date -d "$D1" +%s 2>/dev/null); S2=$(date -d "$D2" +%s 2>/dev/null)
    if [ -n "$S1" ] && [ -n "$S2" ]; then
      DAYS=$(( (S2 - S1) / 86400 ))
      if [ "$DAYS" -gt 0 ] 2>/dev/null; then
        SPAN=$DAYS
        RATE=$(awk -v a="$U1" -v b="$U2" -v d="$DAYS" 'BEGIN{printf "%.2f", (b-a)/d}')
        # 95% 도달 예상일 — 증가 중일 때만 의미가 있다
        ETA=$(awk -v u="$U2" -v r="$RATE" 'BEGIN{ if (r > 0.02 && u < 95) printf "%d", (95-u)/r }')
      fi
    fi
  fi
fi
if [ -n "$RATE" ]; then
  echo "  디스크 추이: ${SPAN}일간 하루 ${RATE}%p"
  [ -n "$ETA" ] && echo "  95% 도달 예상: 약 ${ETA}일 후"
else
  echo "  디스크 추이: 데이터 부족 (감시 기록 2일 이상 필요)"
fi
echo "HM_DASH_DISKRATE=${RATE}"
echo "HM_DASH_DISKETA=${ETA}"
echo "HM_DASH_DISKSPAN=${SPAN}"

# 2) 백업 위치가 데이터 파티션과 같은지 — 계정 백업이 디스크를 채울 수 있다
BSRC=$(df -P /backup 2>/dev/null | awk 'NR==2{print $1}')
HSRC=$(df -P /home 2>/dev/null | awk 'NR==2{print $1}')
BSAME=0
if [ -n "$BSRC" ] && [ "$BSRC" = "$HSRC" ]; then BSAME=1; fi
if [ -z "$BSRC" ]; then
  echo "  백업 경로: /backup 없음 (v-backup-user 가 실패할 수 있음)"
else
  echo "  백업 경로: $BSRC $([ "$BSAME" = 1 ] && echo '← /home 과 같은 파티션' || echo '(별도 파티션)')"
fi
echo "HM_DASH_BACKUPSAME=$BSAME"
echo "HM_DASH_BACKUPSRC=${BSRC:-}"

# 3) PHP-FPM 버전 공존 — 도메인 버전 변경 후 구버전을 안 내리면 소켓을 계속 잡는다
PHPV=$(systemctl list-units --type=service 'php*-fpm.service' --no-legend 2>/dev/null \
  | awk '{print $1}' | grep -oE '[0-9]+\.[0-9]+' | sort -u | tr '\n' ' ' | sed 's/ *$//')
NPV=$(printf '%s' "$PHPV" | wc -w)
echo "  PHP-FPM 버전: ${PHPV:-없음} (${NPV}개)"
echo "HM_DASH_PHPVERS=${PHPV}"
echo "HM_DASH_PHPVERN=${NPV}"

# 4) 버전을 넘나드는 소켓 중복 — 2026-07-25 장애의 실제 원인이었다
DUP=$(grep -rhE '^[[:space:]]*listen[[:space:]]*=' /etc/php/*/fpm/pool.d/*.conf 2>/dev/null \
  | sed 's/.*=[[:space:]]*//' | sort | uniq -d | grep -c .)
[ -z "$DUP" ] && DUP=0
echo "  소켓 중복 정의: ${DUP}건"
echo "HM_DASH_SOCKDUP=$DUP"
```

검증한 동작:

| 입력 | 출력 |
|---|---|
| 13일간 66→69% | `하루 0.23%p`, `95% 도달 약 113일 후` |
| 기록 1행만 | `데이터 부족`, RATE/ETA 빈 값 |
| 감소 추세(80→69%) | `하루 -1.10%p`, ETA 빈 값 |
| 이미 95% 초과 | RATE 계산, ETA 빈 값 |

이 값(`HM_DASH_DISKRATE`)은 `history.tsv` 의 `use%` 를 쓰는데 그건 **그날의 최대 사용률 하나**라
디스크별로 나뉘지 않는다. **디스크별 추이는 §3-1-2 에서 따로 만든다.**

둘 다 유지한다: 감시가 오래 돌아 `history.tsv` 는 쌓였는데 `disk-usage.tsv` 는 이제 시작하는
전환기가 있기 때문이다. **표시는 디스크별 추이를 우선**하고, 없을 때만 전체 추이를 쓴다.

### 3-1-1. 디스크별 수집 — 같은 스냅샷 안에 이어서 추가

**실제 실행 검증했다(2디스크 시나리오 포함). 그대로 쓸 것.**

```sh
echo "[디스크별 헬스]"

# 물리 디스크 SMART 는 파티션마다 다시 물으면 느리다. 한 번 조회해 캐시한다.
SMCACHE=$(mktemp)
smart_of() {   # $1=물리디스크명(sda) → "상태 realloc pending 가동h 온도"
  local D="$1" line
  line=$(grep -m1 "^$D " "$SMCACHE" 2>/dev/null)
  if [ -n "$line" ]; then printf '%s' "${line#* }"; return; fi
  local H="-" RS="-" PS="-" POH="-" TMP="-"
  if command -v smartctl >/dev/null 2>&1 && [ -b "/dev/$D" ]; then
    H=$(smartctl -H "/dev/$D" 2>/dev/null | grep -iE 'overall-health|SMART Health Status' | sed 's/.*: *//' | tr -d ' ')
    [ -z "$H" ] && H="-"
    local A
    A=$(smartctl -A "/dev/$D" 2>/dev/null)
    RS=$(printf '%s' "$A" | awk '/Reallocated_Sector_Ct/{print $10; exit}')
    PS=$(printf '%s' "$A" | awk '/Current_Pending_Sector/{print $10; exit}')
    POH=$(printf '%s' "$A" | awk '/Power_On_Hours/{print $10; exit}')
    TMP=$(printf '%s' "$A" | awk '/Temperature_Celsius|Airflow_Temperature/{print $10; exit}')
    # NVMe 는 속성 이름이 다르다
    if [ "$RS" = "" ] || [ "$POH" = "" ]; then
      local N
      N=$(smartctl -A "/dev/$D" 2>/dev/null)
      [ -z "$POH" ] && POH=$(printf '%s' "$N" | awk -F: '/Power On Hours/{gsub(/[ ,]/,"",$2); print $2; exit}')
      [ -z "$TMP" ] && TMP=$(printf '%s' "$N" | awk -F: '/Temperature:/{gsub(/[^0-9]/,"",$2); print $2; exit}')
      [ -z "$RS" ] && RS=$(printf '%s' "$N" | awk -F: '/Available Spare:/{gsub(/[^0-9]/,"",$2); print "spare"$2; exit}')
    fi
  fi
  [ -z "$RS" ] && RS="-"; [ -z "$PS" ] && PS="-"; [ -z "$POH" ] && POH="-"; [ -z "$TMP" ] && TMP="-"
  echo "$D $H $RS $PS $POH $TMP" >> "$SMCACHE"
  printf '%s' "$H $RS $PS $POH $TMP"
}

DUSAGE=/var/lib/hm-disk-monitor/disk-usage.tsv
trend_of() {   # $1=마운트포인트 → "하루당%p 95%도달일 관측일수" (없으면 "- - -")
  local MP="$1" ROWS N F L D1 U1 D2 U2 S1 S2 DAYS RATE ETA
  [ -s "$DUSAGE" ] || { printf -- '- - -'; return; }
  ROWS=$(awk -F'\t' -v m="$MP" '$1!="date" && $2==m' "$DUSAGE" 2>/dev/null | tail -60)
  N=$(printf '%s\n' "$ROWS" | grep -c .)
  if [ "${N:-0}" -lt 2 ] 2>/dev/null; then printf -- '- - -'; return; fi
  F=$(printf '%s\n' "$ROWS" | head -1); L=$(printf '%s\n' "$ROWS" | tail -1)
  D1=$(printf '%s' "$F" | cut -f1); U1=$(printf '%s' "$F" | cut -f4)
  D2=$(printf '%s' "$L" | cut -f1); U2=$(printf '%s' "$L" | cut -f4)
  S1=$(date -d "$D1" +%s 2>/dev/null); S2=$(date -d "$D2" +%s 2>/dev/null)
  if [ -z "$S1" ] || [ -z "$S2" ]; then printf -- '- - -'; return; fi
  DAYS=$(( (S2 - S1) / 86400 ))
  if [ "$DAYS" -le 0 ] 2>/dev/null; then printf -- '- - -'; return; fi
  RATE=$(awk -v a="$U1" -v b="$U2" -v d="$DAYS" 'BEGIN{printf "%.2f", (b-a)/d}')
  ETA=$(awk -v u="$U2" -v r="$RATE" 'BEGIN{ if (r > 0.02 && u < 95) printf "%d", (95-u)/r }')
  printf -- '%s %s %s' "$RATE" "${ETA:--}" "$DAYS"
}

NDISK=0
while read -r FS SZ USED AVAIL PCT MP; do
  case "$FS" in /dev/*) ;; *) continue ;; esac
  case "$MP" in /boot*|/efi*) continue ;; esac      # 부팅 파티션은 운영 지표가 아니다
  P=${PCT%\%}
  # inode 사용률
  IP=$(df -iP "$MP" 2>/dev/null | awk 'NR==2{gsub(/%/,"",$5); print $5}'); [ -z "$IP" ] && IP="-"
  # 물리 디스크
  PK=$(lsblk -no PKNAME "$FS" 2>/dev/null | head -1 | tr -d ' ')
  [ -z "$PK" ] && PK=$(basename "$FS")
  # ext 계열이면 누적 FS 에러
  FE="-"
  if [ -b "$FS" ]; then
    FE=$(tune2fs -l "$FS" 2>/dev/null | awk -F: '/FS Error count/{gsub(/ /,"",$2); print $2; exit}')
    [ -z "$FE" ] && FE="-"
  fi
  # 역할 — 백업 디스크가 차면 백업이 실패하므로 구분해서 보여준다
  ROLE=other
  case "$MP" in
    /backup*|*backup*|*Backup*) ROLE=backup ;;
    /|/home|/home/*|/var|/var/*) ROLE=main ;;
  esac
  SM=$(smart_of "$PK")
  TR=$(trend_of "$MP")
  NDISK=$((NDISK + 1))
  RATE=$(printf '%s' "$TR" | awk '{print $1}'); ETA=$(printf '%s' "$TR" | awk '{print $2}')
  TRTXT=""
  [ "$RATE" != "-" ] && TRTXT="  추이 ${RATE}%p/일"
  [ "$ETA" != "-" ] && [ -n "$ETA" ] && TRTXT="$TRTXT (95% 약 ${ETA}일 후)"
  printf '  %-14s %-16s %-7s %4s%%  %6s 남음  inode %3s%%  %s%s\n' "$MP" "$FS" "$ROLE" "$P" "$AVAIL" "$IP" "$SM" "$TRTXT"
  # 마커 16필드: 마운트포인트 장치 물리디스크 역할 사용% 남은 inode% FS에러 SMART realloc pending 가동h 온도 추이 95%도달일 관측일수
  printf 'HM_DISK %s %s %s %s %s %s %s %s %s %s\n' "$MP" "$FS" "$PK" "$ROLE" "$P" "$AVAIL" "$IP" "$FE" "$SM" "$TR"
done < <(df -hP 2>/dev/null | awk 'NR>1')

[ "$NDISK" = 0 ] && echo "  (검사 가능한 디스크를 찾지 못했습니다)"
echo "HM_DASH_DISKN=$NDISK"
rm -f "$SMCACHE"
```

검증한 동작 (운영 `/home` 82%, 백업 `/backup` 94% + SMART 불량 시나리오):

```
  /home     /dev/sda1  main    82%  170G 남음  inode  9%  PASSED  0  0 43800 41  추이 0.21%p/일 (95% 약 61일 후)
HM_DISK /home /dev/sda1 sda main 82 170G 9 0 PASSED 0 0 43800 41 0.21 61 14
  /backup   /dev/sdb1  backup  94%  110G 남음  inode  1%  FAILED! 48 12 43800 41  추이 0.43%p/일 (95% 약 2일 후)
HM_DISK /backup /dev/sdb1 sdb backup 94 110G 1 3 FAILED! 48 12 43800 41 0.43 2 14
HM_DASH_DISKN=2
```

**디스크마다 증가 속도가 다르게 나온다** — 위 예에서 `/home` 은 61일 여유인데
`/backup` 은 2일 뒤 꽉 찬다. 최대값 하나로 뭉쳤다면 절대 드러나지 않을 정보다.

※ 위 값들은 **로직 검증용으로 일부러 나쁘게 만든 시나리오**다. 실제 대상 서버를 같은
스크립트로 돌린 결과는 아래와 같다(추이는 `disk-usage.tsv` 가 아직 없어 `-`):

```
  /              /dev/nvme1n1p2   main      14%    383G 남음  inode   1%  PASSED 0 0 9100 44
HM_DISK / /dev/nvme1n1p2 nvme1n1 main 14 383G 1 0 PASSED 0 0 9100 44 - - -
  /home          /dev/nvme0n1p1   main      69%    142G 남음  inode   2%  PASSED 0 0 9100 44
HM_DISK /home /dev/nvme0n1p1 nvme0n1 main 69 142G 2 0 PASSED 0 0 9100 44 - - -
  /backup        /dev/sda1        backup     7%    3.2T 남음  inode   1%  PASSED 0 0 28500 38
HM_DISK /backup /dev/sda1 sda backup 7 3.2T 1 0 PASSED 0 0 28500 38 - - -
HM_DASH_DISKN=3
```

`tmpfs`·`efivarfs`·`/boot/efi` 가 정상적으로 제외되고, 파티션이 각각 다른 물리 디스크
(`nvme1n1`/`nvme0n1`/`sda`)로 매핑되는 것을 실측으로 확인했다. **디스크 개수를 가정하지 말 것**
— 2개일 수도 3개일 수도 있다.

설계 메모:

- `HM_DISK` 는 도메인 헬스의 `HM_DOMH` 와 같은 "한 줄에 한 건" 형식이다. 파싱도 같은 방식.
  필드는 `마운트포인트 장치 물리디스크 역할 사용% 남은용량 inode% FS에러 SMART realloc pending 가동h 온도 추이 95%도달일 관측일수` **16개**.
- SMART 는 **물리 디스크** 단위다. 파티션(`/dev/sda1`)에서 `lsblk -no PKNAME` 으로 부모(`sda`)를
  찾아 조회하고, 같은 디스크를 두 번 묻지 않도록 캐시한다(파티션이 여러 개면 느려진다).
- `/boot`·`/efi` 는 운영 지표가 아니라 제외한다. 실측에서 `/boot/efi` 가 섞여 나왔다.
- 역할(`main`/`backup`/`other`)은 마운트포인트로 판별한다. 오판이 있어도 **표시용**이며
  등급 계산은 역할과 무관하다(백업 가중은 권고에서만 쓴다).
- NVMe 는 SMART 속성 이름이 SATA 와 달라 폴백을 넣었다. 값이 없으면 `-` 로 나가고
  §2-2-1 규칙에 따라 그 세부만 건너뛴다.

### 3-2. `ServerSnapshot` 필드 추가

```rust
#[serde(default)] pub disk_rate: String,     // 하루당 %p (빈 문자열 = 데이터 부족)
#[serde(default)] pub disk_eta: String,      // 95% 도달 예상일 (빈 문자열 = 해당 없음)
#[serde(default)] pub disk_span: String,     // 추이 계산에 쓴 일수
#[serde(default)] pub backup_same: String,   // "1" = /backup 이 /home 과 같은 파티션
#[serde(default)] pub backup_src: String,    // /backup 의 장치 (빈 문자열 = /backup 없음)
#[serde(default)] pub php_vers: String,      // "7.4 8.4"
#[serde(default)] pub php_vern: String,      // 버전 개수
#[serde(default)] pub sock_dup: String,      // 소켓 중복 정의 건수
```

마커 이름은 기존 파싱과 같은 방식(`HM_DASH_` 접두어 제거 후 키)으로 자동 매핑된다:
`DISKRATE`, `DISKETA`, `DISKSPAN`, `BACKUPSAME`, `BACKUPSRC`, `PHPVERS`, `PHPVERN`, `SOCKDUP`, `DISKN`.

디스크별 항목은 별도 구조체로 받는다(`HM_DISK` 줄, `DomainHealth` 와 같은 패턴):

```rust
/// 디스크 1건의 헬스 (마커 HM_DISK 한 줄)
#[derive(Default, Serialize, Deserialize, Clone)]
pub struct DiskHealth {
    #[serde(default)] pub mount: String,     // /home
    #[serde(default)] pub device: String,    // /dev/sda1
    #[serde(default)] pub disk: String,      // sda (물리 디스크)
    #[serde(default)] pub role: String,      // "main"|"backup"|"other"
    #[serde(default)] pub use_pct: String,   // 82
    #[serde(default)] pub avail: String,     // 170G
    #[serde(default)] pub inode_pct: String, // 9   ("-" 가능)
    #[serde(default)] pub fs_err: String,    // 0   ("-" 가능)
    #[serde(default)] pub smart: String,     // PASSED / FAILED! / "-"
    #[serde(default)] pub realloc: String,   // "-" 가능
    #[serde(default)] pub pending: String,   // "-" 가능
    #[serde(default)] pub power_h: String,   // "-" 가능
    #[serde(default)] pub temp_c: String,    // "-" 가능
    #[serde(default)] pub rate: String,      // 하루당 %p ("-" = 데이터 부족)
    #[serde(default)] pub eta: String,       // 95% 도달 예상일 ("-" = 해당 없음)
    #[serde(default)] pub span: String,      // 추이 관측 일수
}
```

`Store` 에 `#[serde(default)] pub disk_health: Vec<DiskHealth>` 추가.
스냅샷 조회 성공 시(`Done{ok:true}`) 통째로 교체한다.

### 3-3. 권고 목록 (등급과 별개)

등급은 "지금 상태"고, 권고는 "무엇을 하라"다. 둘을 섞지 말고 카드를 나눈다.

```rust
/// 캐시된 스냅샷·도메인 헬스에서 실행 가능한 권고를 만든다. SSH 를 새로 쏘지 않는다.
fn advisories(store: &Store) -> Vec<(String /*문구*/, Option<&'static str> /*바로가기 라벨*/)>
```

| 조건 | 문구 | 바로가기 |
|---|---|---|
| 디스크별 `eta` 가 있고 <=30 | `M 디스크가 약 N일 후 95% 도달 예상 (하루 R%p) — 정리가 시급합니다` | 디스크 점검 |
| 디스크별 `eta` 가 있고 <=90 | `M 디스크가 약 N일 후 95% 도달 예상 — 정리 계획이 필요합니다` | 디스크 점검 |
| 위가 없고 `disk_eta` <=60 (전체 추이 폴백) | `디스크(M 기준)가 약 N일 후 95% 도달 예상` | 디스크 점검 |
| `disk_rate` 가 비었고 `diskmon=="1"` | `디스크 추이 데이터가 부족합니다 — 며칠 더 쌓이면 증가 속도를 알 수 있습니다` | — |
| `backup_src` 가 빈 문자열 | `/backup 이 없습니다 — 계정 백업(v-backup-user)이 실패할 수 있습니다` | — |
| `backup_same == "1"` | `/backup 이 /home 과 같은 파티션입니다 — 큰 계정 백업 시 디스크를 채울 수 있습니다` | — |
| 역할 `backup` 디스크가 D 이하 | `백업 디스크 M 상태 D — 백업이 실패하면 복구 경로가 막힙니다` | 디스크 점검 |
| 역할 `backup` 사용률 >=90% | `백업 디스크 M 사용률 P% — v-backup-user 가 곧 실패합니다` | 디스크 점검 |
| SMART 가 `PASSED` 아닌 디스크 | `디스크 D SMART 이상(S) — 교체를 검토하세요` | 디스크 점검 |
| 재할당+대기 합계 >=10 | `디스크 D 불량섹터 N개 — 늘어나면 교체 신호입니다` | 점검 기록 보기 |
| `php_vern` ≥ 2 | `PHP-FPM 버전이 N개 공존합니다 (V) — 도메인 버전 변경 후 구버전도 재시작하세요` | PHP-FPM 진단 |
| `sock_dup` > 0 | `PHP-FPM 소켓 중복 정의 N건 — 2026-07-25 장애의 원인이었습니다` | PHP-FPM 진단 |
| `diskmon == "0"` | `디스크 자동 감시가 설치되지 않았습니다` | 디스크 점검 |
| `trafficmon == "0"` | `트래픽 감시가 설치되지 않았습니다` | — |
| `domain_health.is_empty()` | `도메인 헬스를 아직 점검하지 않았습니다 — 서버가 멀쩡해도 개별 사이트는 죽어 있을 수 있습니다` | 도메인 점검 |
| `cert_soon > 0` | `인증서 D-14 이내 N건 — 자동 갱신이 실패하고 있을 수 있습니다` | 아래 목록 |
| `cert_soon > 0` 이면서 같은 도메인이 `DNS=other` | `N건은 DNS 가 다른 서버를 가리켜 갱신 검증이 실패한 것으로 보입니다` | 아래 목록 |

- 권고가 없으면 `조치할 항목이 없습니다` 를 초록으로.
- 바로가기는 **기존 화면 전환 코드만** 호출한다. 새 SSH 를 쏘지 않는다.
- 권고 문구는 왜 문제인지까지 담는다. "디스크 69%" 만으로는 사용자가 판단할 수 없다.

---

### 3-1-2. 디스크별 추이의 원천 — 감시 스크립트에 일일 기록 추가

디스크별 추이를 내려면 **디스크마다 매일 사용률이 기록되어야** 한다. 지금 감시 스크립트
(`hm-disk-monitor.sh`, `DISK_MONITOR_INSTALL_BODY` 안)는 `history.tsv` 에 최대값 하나만 남긴다.

**`[용량]` 섹션에 아래를 추가한다.** 기존 `history.tsv` 는 건드리지 않는다(하위호환).

```sh
DUSAGE="$STATE/disk-usage.tsv"
mkdir -p "$STATE"

[ -s "$DUSAGE" ] || printf 'date\tmount\tdevice\tuse\tavail\n' > "$DUSAGE"
# 같은 날 재실행이면 그날 줄을 갈아끼운다(하루 디스크당 1행 유지)
if grep -q "^$DAY"$'\t' "$DUSAGE" 2>/dev/null; then
  grep -v "^$DAY"$'\t' "$DUSAGE" > "$DUSAGE.tmp" && mv "$DUSAGE.tmp" "$DUSAGE"
fi

while read -r FS SZ USED AVAIL PCT MP; do
  case "$FS" in /dev/*) ;; *) continue ;; esac
  case "$MP" in /boot*|/efi*) continue ;; esac
  printf '%s\t%s\t%s\t%s\t%s\n' "$DAY" "$MP" "$FS" "${PCT%\%}" "$AVAIL" >> "$DUSAGE"
done < <(df -hP 2>/dev/null | awk 'NR>1')

# 오래된 기록 정리 — 디스크 3개 * 3년이면 3천 줄 남짓이라 부담은 없지만 무한 증가는 막는다
LINES=$(grep -c . "$DUSAGE" 2>/dev/null)
if [ "${LINES:-0}" -gt 5000 ] 2>/dev/null; then
  { head -n 1 "$DUSAGE"; tail -n 4000 "$DUSAGE" | grep -v '^date'; } > "$DUSAGE.tmp" && mv "$DUSAGE.tmp" "$DUSAGE"
fi
```

- 파일: `/var/lib/hm-disk-monitor/disk-usage.tsv` — `날짜 · 마운트포인트 · 장치 · 사용률 · 남은용량`
- 하루에 디스크당 1행. 같은 날 재실행하면 그날 줄을 갈아끼운다
- 5000줄을 넘으면 최근 4000줄만 남긴다(디스크 3개 × 3년이면 3천 줄 남짓이라 여유롭다)
- **감시를 다시 설치해야 적용된다.** 설치 후 최소 2일이 지나야 추이가 나온다
  (그전에는 `-` 로 나오고 UI 는 "데이터 쌓이는 중" 으로 표시한다)

검증한 동작: 1일차 기록 → 같은 날 재실행 시 줄 수 유지(중복 없음) → 2·3일차 누적.

## 4. Phase 6-C — 대시보드 UX 정리 (전체 스크롤 + 목록)

### 4-0. 대시보드 전체 스크롤 (먼저 할 것)

현재 `dashboard_page` 는 `CentralPanel` 안에 카드를 바로 쌓고 **`ScrollArea` 가 없다.**
카드가 화면 높이를 넘으면 아래가 잘려서 보이지 않는다. Phase 6-A/6-B 로 카드가 더 늘어나므로
**이걸 먼저 고치고 나머지를 얹는다.**

```rust
egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
    egui::ScrollArea::vertical()
        .id_salt("dashboard_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // 기존 카드들
        });
});
```

주의:

- **중첩 스크롤**: 안쪽 도메인 목록(§4-1)은 `max_height` 가 고정이라 공존해도 되지만,
  두 `ScrollArea` 모두 **서로 다른 `id_salt`** 를 반드시 준다. 없으면 스크롤 위치가 엉킨다.
- 안쪽 목록에 마우스를 올리면 바깥이 안 움직이는 게 정상이다. 목록 밖 여백에서 스크롤하면
  바깥이 움직인다. 이 동작이 헷갈리면 안쪽 `max_height` 를 키우고 바깥에만 스크롤을 둬도 된다.

### 4-0-1. 좁은 창 대응

지표 카드를 `ui.columns(3, ...)` 로 배치하면 창이 좁을 때 글자가 잘린다.

```rust
let narrow = ui.available_width() < 900.0;
if narrow { /* 세로로 하나씩 */ } else { ui.columns(3, ...) }
```

기준값 900.0 은 고정으로 시작한다. 창 크기에 따라 열 수를 계산하려 들면 레이아웃이 튄다.

### 4-1. 도메인 헬스 목록 스크롤·복사

현재 `egui::Grid` 만 쓰고 **`ScrollArea` 가 없어** 도메인이 많으면 화면을 넘어간다.
복사 수단도 없다. 실제로 97개 환경에서 쓸 수 없는 상태다.

### 4-1-1. 목록 스크롤

기존 패턴(`app.rs` 의 `ScrollArea::vertical().auto_shrink([false,false]).max_height(...)`)을 따른다.

```rust
egui::ScrollArea::vertical()
    .id_salt("dash_domain_health_scroll")   // Grid 와 ID 충돌을 피한다
    .auto_shrink([false, false])
    .max_height(320.0)
    .show(ui, |ui| { /* 기존 Grid */ });
```

- `max_height` 는 고정 320.0 으로 시작한다. 창 높이에 맞추려 계산하면 다른 카드와 어긋난다.
- 이상 목록만 스크롤한다(요약 줄은 스크롤 밖에 고정).

### 4-1-2. 복사

카드 우측 상단에 버튼 2개. 기존 선례: `ui.ctx().copy_text(String)` (`app.rs` 3곳에서 사용 중).

| 버튼 | 내용 |
|---|---|
| `이상만 복사` | 이상 도메인만 |
| `전체 복사` | 점검한 모든 도메인 |

형식은 **탭 구분(TSV)** — 엑셀·스프레드시트에 그대로 붙는다. 첫 줄은 헤더.

```
계정	도메인	사유	DNS	A레코드	공개응답	인증서D-day	웹루트
rokmc	example.com	DNS가 다른 서버(1.2.3.4)	other	1.2.3.4	200	4	yes
```

정상 도메인의 `사유` 칸은 빈 값으로 둔다. 복사 후 `ui.ctx()` 로 상태 메시지
(`"이상 3건을 클립보드에 복사했습니다"`)를 `self.status` 에 남긴다.

### 4-1-3. 텍스트 선택 허용

셀 값을 드래그로 집을 수 있게 한다. 도메인명·A레코드처럼 부분 복사가 잦다.

```rust
ui.add(egui::Label::new(&health.a_record).selectable(true));
```

도메인 칸은 지금처럼 `ui.link()` 로 두고(계정 관리 이동), 나머지 칸에 적용한다.

### 4-2. 테스트

```rust
#[test]
fn domain_health_tsv_export() {
    // 헤더가 8칸이고 탭으로 구분된다
    // 정상 도메인의 사유 칸이 빈 값이다
    // 이상만 내보내면 정상 도메인이 빠진다
    // 값에 탭·개행이 섞여도 열이 깨지지 않는다(공백으로 치환)
}
```

마지막 항목이 중요하다 — `사유` 문구에 `·` 를 쓰고 있어 지금은 안전하지만,
값에서 `\t`/`\n` 을 공백으로 치환하는 방어를 넣는다.

---

## 5. 함정

1. **등급 계산이 SSH 를 유발하면 안 된다.** 캐시만 읽는다. 캐시가 없으면 등급 없음(`None`).
2. **평균으로 총합을 내지 말 것.** 서비스 정지 + 나머지 A → 총합은 **E** 다.
3. **미조회를 좋은 등급으로 표시하지 말 것.** 도메인 미점검 상태의 A 는 거짓 안심이다.
4. `ScrollArea` 안에 `Grid` 를 넣을 때 `id_salt` 를 주지 않으면 ID 충돌로 레이아웃이 튄다.
5. `disk_rate`/`disk_eta` 는 **빈 문자열이 정상값**이다(데이터 부족·감소 추세).
   `parse().unwrap_or(0)` 으로 0 처리하면 "하루 0%p 증가" 로 잘못 표시된다. `Option` 으로 다뤄라.
6. 스크립트에 추가한 섹션은 `set +e` 환경이다. `date -d` 가 실패해도 빈 값으로 넘어가야 한다.
7. 권고 문구를 UI 와 스크립트 양쪽에 쓰지 말 것. **문구는 Rust 한 곳**에만 둔다
   (스크립트는 수치만 출력). 두 곳에 있으면 어긋난다.
8. **디스크를 최대값 하나로 뭉치지 말 것.** 운영과 백업은 성격이 다르고, 백업 디스크가 차면
   백업이 실패한다는 사실이 최대값 하나로는 드러나지 않는다.
9. SMART 조회불가(`-`)를 A 로 처리하지 말 것. smartmontools 미설치 서버가 영원히 "우수"가 된다.
10. `HM_DISK` 줄의 필드 수가 16이 아니면 그 줄을 **버린다**. 마운트포인트에 공백이 들어가면
    열이 밀려 엉뚱한 값이 등급에 들어간다.

---

## 6. DoD

- [ ] `cargo test` 전부 통과 / `cargo build --release` 경고 없음
- [ ] 등급 카드가 대시보드 최상단에 보이고, 미조회 상태에서 `?` 로 표시된다
- [ ] 서비스 정지를 넣으면 총합이 **E** 가 된다(평균이 아님을 확인)
- [ ] 도메인 미점검 상태에서 `도메인 미점검` 이 병기되고 도메인·인증서 항목이 등급에서 제외된다
- [ ] 감시 미설치만 나쁠 때 총합이 C 보다 나빠지지 않는다
- [ ] 추이 마커 8개가 스냅샷에서 출력된다(`bash -n` + 가능하면 실제 실행)
- [ ] 권고 목록이 조건대로 뜨고, 없으면 초록 안내가 보인다
- [ ] 도메인 헬스 목록이 **스크롤되고**, `이상만 복사`/`전체 복사` 가 TSV 로 동작한다
- [ ] 셀 텍스트를 드래그로 선택할 수 있다
- [ ] **대시보드 전체가 스크롤된다** — 카드를 다 추가한 뒤 창을 작게 줄여도 아래 카드에 닿는다
- [ ] 바깥·안쪽 `ScrollArea` 의 `id_salt` 가 서로 다르다
- [ ] 창을 좁히면 지표 카드가 세로로 배치된다
- [ ] 인증서 임박 문구가 "자동 갱신 실패 가능성" 으로 표시된다(단순 "곧 만료" 아님)
- [ ] **디스크가 2개면 2줄로** 보이고 각각 등급·SMART·inode 가 표시된다
- [ ] 운영 A + 백업 E 상황에서 디스크 항목 등급이 **E** 가 된다(최대값 뭉개기 아님)
- [ ] SMART `-` 인 디스크가 C 로 잡힌다(A 가 아님)
- [ ] 백업 디스크가 90% 이상이거나 D 이하면 권고에 뜬다
- [ ] **디스크마다 추이가 따로** 표시된다(운영 61일 / 백업 2일처럼 다른 값)
- [ ] 감시 스크립트가 `disk-usage.tsv` 를 하루 디스크당 1행으로 남긴다(같은 날 재실행 시 중복 없음)
- [ ] `disk-usage.tsv` 가 없거나 1일치뿐이면 `추이 데이터 쌓이는 중` 으로 표시된다
- [ ] `docs/dashboard.md` 에 등급표(§2-2)·디스크별 규칙(§2-2-1)·총합 규칙(§2-3)·복사 형식을 추가
