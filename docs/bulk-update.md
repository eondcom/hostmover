# 일괄 업데이트 — 안정 릴리스 원칙

서버의 모든 사이트를 한 번에 올리는 기능의 정책과 사용법.

---

## 1. 원칙: master 가 아니라 릴리스 태그

**`master`(또는 `main`)는 개발 trunk 다. 프로덕션에 올리면 안 된다.**

- `WordPress/WordPress` 의 master → 다음 버전의 nightly(알파)
- `rhymix/rhymix`, `gnuboard/gnuboard5` 의 master → 개발 중인 코드

그래서 업데이트는 이렇게 한다:

| CMS | 방식 | 기준 |
|---|---|---|
| WordPress | wp-cli (`core`/`plugin`/`theme`/`language update`) | wp.org 안정 릴리스 |
| git 설치본 | `fetch tag` + `reset --hard <태그>` | `x.y[.z]` 형태의 최신 릴리스 태그 |
| 비-git Rhymix/그누보드 | `clone --branch <태그>` 후 rsync 오버레이 | 최신 릴리스 태그 |

릴리스 태그가 아예 없는 저장소만 브랜치로 폴백하고, 그 사실을 로그와 보고서에 남긴다.

태그 선택은 `ops.rs` 의 `hm_latest_tag` 가 한다:

```bash
git ls-remote --tags --refs <저장소> \
  | awk -F/ '{print $NF}' \
  | grep -E '^v?[0-9]+\.[0-9]+(\.[0-9]+)?$' \
  | sort -V | tail -n 1
```

`-rc1`, `-beta` 같은 접미사가 붙은 태그는 정규식에서 걸러진다.

실측(2026-07-25): rhymix `2.1.35` · gnuboard5 `v5.7.5` · xe-core `1.11.22` · WordPress `7.0.2`

### 개발버전이 이미 올라간 WordPress

`wp core version` 이 `alpha`/`beta`/`RC`/`-src` 를 포함하면, api.wordpress.org 가 알려주는
안정 릴리스로 `--force` 되돌린다. 버전 번호상으로는 알파가 더 높아서 그냥 `core update` 로는
내려가지 않기 때문이다.

---

## 2. 2026-07-25 사고 — WordPress 4곳이 nightly 로 덮임

`설정 > 일괄 업데이트` 버튼이 계정별 업데이트와 **다른 코드**를 돌리고 있었다.

```bash
# 옛 build_bulk_git_update — CMS 를 가리지 않는다
if [ -d "$WR/.git" ]; then
  git fetch --depth=1 origin "$BR" && git reset --hard FETCH_HEAD
```

`.git` 만 있으면 origin 이 어디든 강제로 당겼다. 그래서 origin 이 `WordPress/WordPress` 인
사이트가 개발 trunk 로 덮였다:

- health.logicmaru.co.kr · dental.eond.com · dev.koreanhub24.com · koreanhub.oiii.kr

계정별 업데이트가 쓰던 `SITE_UPDATE_STEP` 에는 원래 `wp-load.php` 를 먼저 보는 올바른 분기가
있었다. **두 경로가 갈라져 있었던 것**이 원인이라, 지금은 하나로 합쳤다.

교훈: 같은 일을 하는 코드가 두 벌 있으면 한쪽만 고쳐진다.

### 남은 뒷정리

nightly 로 덮인 사이트에는 `.git`(origin=WordPress/WordPress)이 남아 있다. 그대로 두면
누군가 git 으로 다시 당길 수 있으므로, 사이트가 정상인지 확인한 뒤 `.git` 을 지우는 게 좋다.
업데이트를 돌리면 보고서에 `※ .git(WordPress) 남아 있음 — 제거 권장` 으로 표시된다.

---

## 3. 결과 보고서 / 메일

작업이 끝나면 로그 맨 아래에 요약 표가 나온다. 수백 줄을 다 읽을 필요 없이 이것만 보면 된다.

```
════════════════════════════════════════════════════════════════════
 전체 사이트 일괄 업데이트 (안정 릴리스)
 mars.eond.com · 2026-07-25 08:01:45
════════════════════════════════════════════════════════════════════
  성공 4    건너뜀 1    실패 1
────────────────────────────────────────────────────────────────────
  사이트                           방식        결과   버전
────────────────────────────────────────────────────────────────────
  dental.eond.com                  wp          OK     7.1-alpha → 7.0.2  ※ 개발버전→안정 되돌림
  eond.com                         git-tag     OK     9eb6931f80 → 2.1.35
  korean.haus                      overlay     OK     - → 2.1.35
  swslr.com                        git-branch  FAIL   9ad995279 → -  ※ 릴리스 태그 없는 저장소
  pub.eond.com                     unknown     SKIP   CMS 를 판별하지 못함
════════════════════════════════════════════════════════════════════
```

방식 표기:

| 값 | 뜻 |
|---|---|
| `wp` | wp-cli 로 처리 |
| `git-tag` | 릴리스 태그로 reset |
| `git-branch` | 태그가 없어 브랜치로 폴백 |
| `overlay` | 비-git 설치본에 최신본 덮어쓰기 |
| `unknown` | CMS 판별 실패 → 손대지 않음 |

**메일**: 카드의 "결과 메일 받기" 에 주소를 넣으면 서버가 같은 보고서를 메일로 보낸다
(`mail` → `sendmail` 순으로 시도). 비워두면 발송하지 않는다. 서버에 둘 다 없으면 그 사실을
로그에 남기고 보고서는 그대로 출력된다.

---

## 4. WordPress 플러그인만 골라서 업데이트

망보드처럼 여러 사이트에 공통으로 깔린 플러그인 하나만 올릴 때.

설정 > 일괄 업데이트 > "WordPress 플러그인 골라서 일괄 업데이트"

1. **슬러그** — `wp-content/plugins/` 아래 폴더 이름 (예: `mangboard`)
2. **대상 계정** — 비우면 서버 전체
3. **점검만** 을 먼저 눌러 어디에 몇 버전이 깔려 있는지 확인 (읽기 전용, 확인 모달 없음)
4. 그다음 **업데이트 실행**

```
  alpha.com                          1.2.0      active  → 1.3.0 있음
  beta.com                           1.2.0      active  → 1.3.0 있음

== 점검 완료: 설치됨 2 · 미설치 1 ==
```

자체 업데이터를 쓰는 상용 플러그인은 wp-cli 로 안 올라갈 수 있고, 그 경우 `FAIL` 로 보고된다.

---

## 5. 관련

- rx-cli(Rhymix CLI) 배포: [rx-cli.md](rx-cli.md)
- hostmover 자체 업데이트: [updating.md](updating.md)
