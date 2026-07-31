# 개발 스펙 — UI 고도화 (Codex 작업 지시서)

작성 2026-07-31. **구현 담당: Codex.** 판단이 끝난 확정 스펙이다.

목표: 화면을 **금융권 서버 관리 콘솔 수준**으로 끌어올린다.

- 대상 브랜치: `dashboard-impl` 위에 이어서 작업
- 선행: [README.md](README.md) 의 지시서들(기능은 이미 구현 완료)

---

## 0. 진단 — 무엇이 "엉성한가"

코드를 실제로 확인한 결과, **색·폰트·간격 체계는 이미 잘 잡혀 있다.**
`main.rs` 의 `setup_theme()` 은 네이비 다크 + 블루 액센트에 텍스트 5단계, 코너 6px,
그림자까지 정의돼 있고 주석에도 "금융권 대시보드 톤" 이라고 적혀 있다.
**테마를 갈아엎을 필요가 없다.** 문제는 아래 4가지다.

| # | 문제 | 결과 |
|---|---|---|
| 1 | **표를 `egui::Grid` 로 그린다** (`egui_extras` 미사용) | 열 너비가 내용에 따라 들쭉날쭉하고, 스크롤하면 헤더가 사라지며, 정렬(클릭 sort)이 안 되고, 97행을 매 프레임 전부 그린다 |
| 2 | **컴포넌트 레이어가 없다** — 공통 헬퍼가 `card()` 하나뿐 | 배지·수치·빈 상태·구분선을 화면마다 직접 조립해 생김새가 제각각이다 |
| 3 | **밀도가 터치 기준** (`interact_size 44x34`, `button_padding 12x8`) | 한 화면에 정보가 적게 들어간다. 관리 콘솔은 밀도가 높아야 한다 |
| 4 | **숫자가 좌측 정렬·가변폭** | 사용률·용량·건수가 세로로 안 맞아 비교가 어렵다 |

**1번이 가장 크다.** 표가 흔들리면 나머지가 아무리 정돈돼도 엉성해 보인다.

### 참고한 것

- [`egui_extras::TableBuilder`](https://docs.rs/egui_extras) — 열 리사이즈·헤더 고정·가상 스크롤
- [Rerun Viewer](https://github.com/rerun-io/rerun) — egui 로 만든 전문가급 앱.
  [`re_ui`](https://docs.rs/re_ui) 라는 **디자인 토큰 + 컴포넌트 레이어**를 따로 두는 구조를 참고했다
- [egui 공식](https://github.com/emilk/egui) · [catppuccin-egui](https://github.com/catppuccin/egui) ·
  [egui-aesthetix](https://github.com/thebashpotato/egui-aesthetix) (테마 프리셋 — 이번엔 쓰지 않는다)

---

## 1. 작업 규칙

- **UI-A → UI-B → UI-C → UI-D 순서로, 각각 커밋을 나눈다.**
- 기능 동작을 바꾸지 않는다. **표시 방식만** 바꾼다.
- 주석·UI 문자열·커밋 메시지는 한국어.

### 절대 규칙

1. **`egui` 버전을 올리지 말 것.** `=0.31.1` 고정이다.
   0.29.x 에 리눅스 IME(한글 입력) 회귀 버그가 있어 고정한 것이고, 올리면 **한글 입력이 깨진다**
   (`README.md` §폰트/한글 입력). `egui_extras` 도 반드시 **`=0.31.1`** 로 맞춘다:

   ```toml
   egui_extras = "=0.31.1"
   ```

   최신은 0.35 지만 egui 와 짝이 맞아야 하므로 쓰면 안 된다.
2. **`setup_theme()` 의 색 팔레트를 바꾸지 말 것.** 이미 의도된 톤이다.
   토큰으로 **정리**는 하되 색값 자체는 유지한다.
3. 기능 로직(`domain_issue`, 등급 산정, 스크립트)을 건드리지 않는다.
4. 자격증명을 소스·테스트에 넣지 않는다.

---

## 2. UI-A — 디자인 토큰과 공통 컴포넌트

지금은 화면마다 색과 간격을 직접 써서 같은 의미가 다르게 보인다. **한 곳에 모은다.**

### 2-1. 토큰 (`src/ui.rs` 신설)

```rust
//! 디자인 토큰과 공통 위젯. 화면 코드는 여기 있는 것만 쓴다.
//! 색·간격을 화면에서 직접 지정하지 않는다 — 그러면 같은 의미가 화면마다 달라진다.

/// 의미 기반 색. setup_theme() 의 팔레트에서 가져온다(값을 새로 만들지 말 것).
pub mod color {
    use egui::Color32;
    pub const OK: Color32       = Color32::from_rgb(0x2E, 0x9E, 0x5B);
    pub const WARN: Color32     = Color32::from_rgb(0xF5, 0xB5, 0x4B);
    pub const DANGER: Color32   = Color32::from_rgb(0xF2, 0x6D, 0x6D);
    pub const ACCENT: Color32   = Color32::from_rgb(0x3B, 0x82, 0xF6);
    pub const MUTED: Color32    = Color32::from_rgb(0x93, 0xA1, 0xBC);
    pub const BORDER: Color32   = Color32::from_rgb(0x39, 0x4A, 0x6D);
}

/// 간격은 4의 배수만 쓴다. 화면에서 7.0 같은 값을 직접 넣지 말 것.
pub mod space {
    pub const XS: f32 = 4.0;
    pub const SM: f32 = 8.0;
    pub const MD: f32 = 12.0;
    pub const LG: f32 = 16.0;
    pub const XL: f32 = 24.0;
}
```

기존 `C_GREEN`/`C_RED` 는 `color::OK`/`color::DANGER` 로 대체하고 **중복 정의를 남기지 않는다.**

### 2-2. 공통 위젯

```rust
/// 상태 배지 — "OK"/"FAIL" 같은 맨 텍스트 대신 쓴다.
pub fn badge(ui: &mut egui::Ui, text: &str, c: egui::Color32);

/// 신호등 점. 등급·상태를 표 왼쪽에 찍는다.
pub fn dot(ui: &mut egui::Ui, c: egui::Color32);

/// 큰 수치 + 라벨 (대시보드 지표 카드용)
pub fn stat(ui: &mut egui::Ui, value: &str, label: &str, c: Option<egui::Color32>);

/// 섹션 제목 + 우측 액션 영역
pub fn section<R>(ui: &mut egui::Ui, title: &str, actions: impl FnOnce(&mut egui::Ui),
                  body: impl FnOnce(&mut egui::Ui) -> R) -> R;

/// 빈 상태 — 아이콘 + 설명 + (선택) 다음 행동 버튼
pub fn empty_state(ui: &mut egui::Ui, icon: &str, msg: &str, hint: Option<&str>);

/// 숫자 셀 — 우측 정렬 + 고정폭(Monospace). 표에서 숫자는 전부 이걸 쓴다.
pub fn num(ui: &mut egui::Ui, text: &str);
```

`badge` 는 배경을 옅게(`c.linear_multiply(0.25)`) 깔고 테두리 없이, 모서리 4px, 좌우 여백 6px.

### 2-3. 적용

대시보드·계정 관리·전체 사이트 화면에서 **직접 쓰던 색/간격을 토큰으로 교체**한다.
`ui.colored_label(C_RED, ...)` → `badge(ui, "...", color::DANGER)` 같은 식.

---

## 3. UI-B — 표를 진짜 테이블로 (가장 중요)

`egui::Grid` → `egui_extras::TableBuilder` 로 바꾼다.

### 3-1. 대상

| 화면 | 현재 | 행 수 |
|---|---|---|
| 대시보드 · 도메인 헬스 | `Grid` | 최대 97 |
| 대시보드 · 확인 권장 사이트 | `Grid` | 수십 |
| 대시보드 · 디스크 목록 | 직접 배치 | 3 |
| 전체 사이트(`all_sites_page`) | `Grid`/직접 | **97+** ← 효과가 가장 크다 |
| 계정 관리 · 사이트/모듈 | `Grid` | 수십 |

### 3-2. 기본형

```rust
use egui_extras::{Column, TableBuilder};

TableBuilder::new(ui)
    .id_salt("domain_health_table")          // 화면마다 고유
    .striped(true)
    .resizable(true)                          // 열 너비를 사용자가 조절
    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
    .column(Column::auto().at_least(180.0))   // 도메인
    .column(Column::remainder())              // 사유 — 남는 폭 전부
    .column(Column::exact(90.0))              // 상태
    .column(Column::exact(80.0))              // 숫자
    .header(24.0, |mut h| {
        h.col(|ui| { ui.strong("도메인"); });
        // ...
    })
    .body(|body| {
        body.rows(22.0, rows.len(), |mut row| {
            let r = &rows[row.index()];
            row.col(|ui| { ui.label(&r.domain); });
            // ...
        });
    });
```

**요구사항**

- `body.rows(...)` 를 쓴다 — 보이는 행만 그리는 **가상 스크롤**이다.
  `body.row()` 를 루프로 돌리면 97행을 전부 그려 지금과 같아진다.
- 행 높이는 **22.0 고정**(밀도). 헤더 24.0.
- 숫자 열은 `Column::exact` + `ui::num()` 으로 **우측 정렬**.
- `resizable(true)` 로 열 폭 조절 허용.
- 헤더는 스크롤해도 고정된다(TableBuilder 기본 동작).

### 3-3. 정렬(sort)

헤더 클릭으로 정렬한다. `TableBuilder` 에 내장 정렬이 없으므로 **직접 구현**한다:

```rust
/// 표 정렬 상태. 화면별로 App 에 하나씩 둔다.
#[derive(Clone, Copy, PartialEq)]
pub struct SortState { pub col: usize, pub desc: bool }
```

- 헤더 셀을 `ui.button()` 으로 만들고 클릭 시 `col` 을 바꾸거나 같은 열이면 `desc` 토글
- 현재 정렬 열에 `▲`/`▼` 표시
- **정렬은 표시용 사본에만 적용**한다. 원본 `Vec` 의 순서를 바꾸면 선택 상태(`sel`)가 어긋난다
- 기본 정렬: 도메인 헬스는 **이상 있는 것 먼저**, 전체 사이트는 도메인 오름차순

### 3-4. 선택 열

계정 관리 사이트 탭처럼 체크박스가 있는 표는 첫 열을 `Column::exact(28.0)` 로 두고
`ui.checkbox(&mut r.sel, "")` 를 넣는다. 행 클릭으로 토글하던 기존 동작은 유지한다.

---

## 4. UI-C — 밀도와 정렬

관리 콘솔은 한 화면에 많이 보여야 한다. 현재 값은 터치 기준에 가깝다.

### 4-1. `setup_theme()` 조정

```rust
style.spacing.item_spacing   = egui::vec2(8.0, 4.0);    // 세로 6 → 4
style.spacing.button_padding = egui::vec2(10.0, 5.0);   // 12x8 → 10x5
style.spacing.interact_size  = egui::vec2(40.0, 28.0);  // 44x34 → 40x28
```

**색·폰트는 건드리지 않는다.** 간격만 줄인다.

이 변경은 **모든 화면에 영향**을 주므로 UI-C 는 반드시 **독립 커밋**으로 하고, 커밋 메시지에
되돌리는 법을 적어둔다(값 3개만 원복하면 된다).

### 4-2. 숫자 표기 규칙

- 표 안의 모든 숫자는 `ui::num()` — 우측 정렬 + `Monospace`
- 퍼센트는 정수(`69%`), 용량은 소수 1자리(`3.2T`), 건수는 정수
- 날짜/시각은 `2026-07-31 09:11` 고정 형식, 상대시간은 괄호로 병기 (`(3시간 전)`)

### 4-3. 창 폭 대응

이미 `dashboard_is_narrow(900.0)` 이 있다. 같은 기준을 **전체 사이트·계정 관리**에도 적용해
좁을 때 열을 숨긴다(우선순위 낮은 열부터: 생성일 → 권한 → 용량).

---

## 5. UI-D — 상태 표현 표준화

같은 의미가 화면마다 다르게 보인다. **하나로 통일**한다.

| 의미 | 표기 | 색 |
|---|---|---|
| 정상/성공 | `● 정상` 또는 배지 `OK` | `color::OK` |
| 주의 | 배지 `주의` | `color::WARN` |
| 위험/실패 | 배지 `위험` / `실패` | `color::DANGER` |
| 미조회/알 수 없음 | 배지 `미조회` | `color::MUTED` |
| 진행 중 | `ui.spinner()` + 회색 텍스트 | `color::MUTED` |

- **등급(A~E)** 은 이미 색이 있다. `ui::dot()` + 등급 글자로 통일한다.
- **"-" 를 그대로 노출하지 말 것.** `조회불가` 또는 `해당 없음` 으로 적는다.
- 위험 표시에 색만 쓰지 말고 **글자도 함께** 넣는다(색각 이상 대응).

---

## 6. 함정

1. **`egui` 버전을 올리면 한글 입력이 깨진다.** `egui_extras` 는 `=0.31.1`.
2. `body.rows()` 대신 `body.row()` 루프를 쓰면 가상 스크롤이 사라져 **지금과 똑같이 느리다.**
3. `TableBuilder` 를 `ScrollArea` **안에 중첩하지 말 것.** 테이블이 자체 스크롤을 갖는다.
   대시보드 전체 스크롤(`dashboard_scroll`) 안에 넣을 때는 테이블에 `.max_scroll_height()` 를
   주어 높이를 제한한다. 안 그러면 스크롤이 두 겹으로 겹쳐 조작이 이상해진다.
4. **정렬 시 원본 순서를 바꾸지 말 것** — 체크박스 선택이 다른 행으로 옮겨간다.
5. `id_salt` 를 표마다 다르게 준다. 같으면 열 폭이 서로 공유돼 튄다.
6. 밀도 변경(UI-C)은 모달·입력 폼에도 적용된다. 폼이 너무 빽빽해지면 **입력 위젯에만**
   `interact_size` 를 개별 지정해 되돌린다.
7. 색 상수를 새로 만들지 말 것. `setup_theme()` 팔레트에서 가져와 `ui::color` 에 모은다.

---

## 7. DoD

- [ ] `cargo test` 전부 통과 / `cargo build --release` 경고 없음
- [ ] `Cargo.toml` 에 `egui_extras = "=0.31.1"`, `egui` 는 `=0.31.1` 그대로
- [ ] `src/ui.rs` 가 생기고, 화면 코드에서 **직접 쓰는 색·간격 리터럴이 사라짐**
      (`grep -n "from_rgb" src/app.rs` 가 거의 비어야 한다)
- [ ] 전체 사이트·도메인 헬스가 `TableBuilder` 로 그려지고 **헤더가 스크롤에 고정**된다
- [ ] 헤더 클릭으로 정렬되고 `▲`/`▼` 가 표시된다
- [ ] 97행에서 스크롤이 버벅이지 않는다(가상 스크롤 확인)
- [ ] 열 너비를 드래그로 조절할 수 있다
- [ ] 표의 숫자가 우측 정렬·고정폭으로 세로가 맞는다
- [ ] 상태 표기가 §5 표대로 통일된다
- [ ] 한글 입력이 여전히 동작한다(**실행해서 직접 확인** — 버전 사고 방지)
- [ ] `docs/` 에 `ui-guide.md` 추가: 토큰·컴포넌트 사용법, 표 만드는 법, 밀도 되돌리는 법
