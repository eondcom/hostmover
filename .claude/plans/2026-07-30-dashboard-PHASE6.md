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
| **디스크** 최대사용률 | <70% | <80% | <88% | <95% | ≥95% | 95%는 서비스 장애 임박. 88%는 대응 시간 확보선 |
| **서비스 정지** | 0개 | — | — | — | 1개 이상 | 하나라도 죽으면 그 자체로 장애다 |
| **PHP-FPM 실패** | 0개 | — | — | 1개 | 2개 이상 | 실패 1개 = 일부 사이트 다운 |
| **도메인 이상** 비율 | 0% | ≤2% | ≤5% | ≤15% | >15% | 테스트 도메인 등 1~2건은 흔한 잡음. 15% 초과는 구조적 문제 |
| **인증서** | 만료0·임박0 | 임박≤2 | 임박≥3 | 만료1 | 만료≥2 | 만료는 즉시 접속 불가 |
| **자동 감시** | 설치+최근 | 설치+지연(36h+) | 미설치 | — | — | 서버 건강이 아니라 운영 준비 상태 |

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
`DISKRATE`, `DISKETA`, `DISKSPAN`, `BACKUPSAME`, `BACKUPSRC`, `PHPVERS`, `PHPVERN`, `SOCKDUP`.

### 3-3. 권고 목록 (등급과 별개)

등급은 "지금 상태"고, 권고는 "무엇을 하라"다. 둘을 섞지 말고 카드를 나눈다.

```rust
/// 캐시된 스냅샷·도메인 헬스에서 실행 가능한 권고를 만든다. SSH 를 새로 쏘지 않는다.
fn advisories(store: &Store) -> Vec<(String /*문구*/, Option<&'static str> /*바로가기 라벨*/)>
```

| 조건 | 문구 | 바로가기 |
|---|---|---|
| `disk_eta` 가 있고 ≤60 | `디스크가 약 N일 후 95% 도달 예상 (하루 R%p) — 정리 계획이 필요합니다` | 디스크 점검 |
| `disk_rate` 가 비었고 `diskmon=="1"` | `디스크 추이 데이터가 부족합니다 — 며칠 더 쌓이면 증가 속도를 알 수 있습니다` | — |
| `backup_src` 가 빈 문자열 | `/backup 이 없습니다 — 계정 백업(v-backup-user)이 실패할 수 있습니다` | — |
| `backup_same == "1"` | `/backup 이 /home 과 같은 파티션입니다 — 큰 계정 백업 시 디스크를 채울 수 있습니다` | — |
| `php_vern` ≥ 2 | `PHP-FPM 버전이 N개 공존합니다 (V) — 도메인 버전 변경 후 구버전도 재시작하세요` | PHP-FPM 진단 |
| `sock_dup` > 0 | `PHP-FPM 소켓 중복 정의 N건 — 2026-07-25 장애의 원인이었습니다` | PHP-FPM 진단 |
| `diskmon == "0"` | `디스크 자동 감시가 설치되지 않았습니다` | 디스크 점검 |
| `trafficmon == "0"` | `트래픽 감시가 설치되지 않았습니다` | — |
| `domain_health.is_empty()` | `도메인 헬스를 아직 점검하지 않았습니다 — 서버가 멀쩡해도 개별 사이트는 죽어 있을 수 있습니다` | 도메인 점검 |
| `cert_soon > 0` | `인증서 만료 임박 N건 — 갱신 실패로 조용히 쌓입니다` | 아래 목록 |

- 권고가 없으면 `조치할 항목이 없습니다` 를 초록으로.
- 바로가기는 **기존 화면 전환 코드만** 호출한다. 새 SSH 를 쏘지 않는다.
- 권고 문구는 왜 문제인지까지 담는다. "디스크 69%" 만으로는 사용자가 판단할 수 없다.

---

## 4. Phase 6-C — 도메인 헬스 목록 UI (스크롤·복사)

현재 `egui::Grid` 만 쓰고 **`ScrollArea` 가 없어** 도메인이 많으면 화면을 넘어간다.
복사 수단도 없다. 실제로 97개 환경에서 쓸 수 없는 상태다.

### 4-1. 스크롤

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

### 4-2. 복사

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

### 4-3. 텍스트 선택 허용

셀 값을 드래그로 집을 수 있게 한다. 도메인명·A레코드처럼 부분 복사가 잦다.

```rust
ui.add(egui::Label::new(&health.a_record).selectable(true));
```

도메인 칸은 지금처럼 `ui.link()` 로 두고(계정 관리 이동), 나머지 칸에 적용한다.

### 4-4. 테스트

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
- [ ] `docs/dashboard.md` 에 등급표(§2-2)와 총합 규칙(§2-3), 복사 형식을 추가
