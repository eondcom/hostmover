# 관리 콘솔 UI 가이드

Hostmover 화면은 운영자가 오래 사용하는 데이터 밀도형 관리 콘솔을 기준으로 한다. 색은 장식이
아니라 상태 신호로만 쓰며, 같은 의미는 모든 화면에서 같은 컴포넌트로 표시한다.

## 토큰과 컴포넌트

`src/ui.rs`의 `color`와 `space`를 사용한다. 화면 코드에서 새 RGB 색을 만들거나 임의 간격을
추가하지 않는다. 간격은 `4 / 8 / 12 / 16 / 24` 중 하나를 사용한다.

- 상태: `badge()` 또는 `dot()`과 상태 글자를 함께 사용한다.
- 수치 카드: `stat()`을 사용한다.
- 섹션: `section()`으로 제목, 우측 액션, 본문 간격을 통일한다.
- 빈 상태: `empty_state()`에 빈 이유와 다음 행동을 함께 적는다.
- 숫자 셀: `num()`으로 우측 정렬과 고정폭 글꼴을 적용한다.

값을 조회하지 않았으면 `미조회`, 조회에 실패했으면 `조회불가`, 적용 대상이 아니면 `해당 없음`을
표시한다. 빈칸이나 `-`는 정상으로 오해할 수 있으므로 화면에 노출하지 않는다.

## 표 만들기

데이터 표는 `egui::Grid`가 아니라 `egui_extras::TableBuilder`를 사용한다. 표마다 고유한
`id_salt`를 지정하고 `striped(true)`, `resizable(true)`를 켠다. 헤더는 24px, 행은 22px로
고정하며 반드시 `body.rows()`로 보이는 행만 그린다.

```rust
TableBuilder::new(ui)
    .id_salt("unique_table")
    .striped(true)
    .resizable(true)
    .column(Column::remainder().at_least(180.0))
    .column(Column::exact(80.0))
    .header(console_ui::TABLE_HEADER_HEIGHT, |mut header| {
        header.col(|ui| { ui.strong("이름"); });
        header.col(|ui| { ui.strong("용량"); });
    })
    .body(|body| {
        body.rows(console_ui::TABLE_ROW_HEIGHT, rows.len(), |mut row| {
            let item = &rows[row.index()];
            row.col(|ui| { ui.label(&item.name); });
            row.col(|ui| console_ui::num(ui, &item.size));
        });
    });
```

정렬은 원본 벡터가 아니라 표시용 인덱스나 사본에만 적용한다. 체크박스가 있는 표에서 원본을
정렬하면 선택이 다른 행으로 이동할 수 있다. 대시보드처럼 바깥 스크롤 안에 표가 있으면
`max_scroll_height()`로 높이를 제한한다.

## 밀도와 되돌리기

전역 밀도는 `src/main.rs`의 `setup_theme()`에 있는 다음 세 값으로 정한다.

```rust
style.spacing.item_spacing = egui::vec2(8.0, 4.0);
style.spacing.button_padding = egui::vec2(10.0, 5.0);
style.spacing.interact_size = egui::vec2(40.0, 28.0);
```

밀도 변경으로 입력 폼이 지나치게 좁아진 경우 전역 값을 바꾸지 말고 해당 입력 위젯에만 최소
높이를 지정한다. 기존 밀도로 되돌려야 한다면 위 세 값을 각각 `8x6`, `12x8`, `44x34`로
원복한다.

## 변경 전 확인

- `egui`, `eframe`, `egui_extras`는 모두 정확히 `=0.31.1`이어야 한다.
- 폰트 등록과 `setup_theme()`의 색 팔레트는 바꾸지 않는다.
- 색이 있는 모든 상태에는 `정상`, `주의`, `위험`, `실패`, `미조회` 같은 글자가 함께 있어야 한다.
- 조회 결과에는 기준 시각을 표시하고, 빈 상태에는 다음 행동을 적는다.
- `cargo test`와 `cargo build --release`가 경고 없이 끝나는지 확인한다.
