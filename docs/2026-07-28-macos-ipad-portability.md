# macOS(인텔 맥북) / 아이패드 지원 검토

- **일자**: 2026-07-28
- **주제**: Hostmover 를 리눅스 외 기기 — 인텔 맥북, 아이패드 — 에서 쓸 수 있게 하는 방법
- **상태**: 논의만 함. 코드 변경 없음. 나중에 착수할 때 참고용.
- **검증 범위**: 코드 정적 분석만. macOS 실기가 없어 실제 빌드는 확인하지 못했다.

---

## 결론 요약

**맥북과 아이패드는 완전히 다른 문제다.**

| 대상 | 판정 | 이유 |
|---|---|---|
| 인텔 맥북 (macOS) | **거의 됨** — 실기에서 빌드만 하면 됨 | 이미 macOS 대응 코드가 들어가 있음 |
| 아이패드 (iPadOS) | **네이티브 포팅 불가** | iOS/iPadOS 가 자식 프로세스 실행을 금지 |

---

## 1. 인텔 맥북 — 이미 절반 이상 되어 있다

### 이미 준비된 것

- `build-macos.sh` — `.app` 번들 + `--universal`(x86_64 + arm64) + `--dmg`, ad-hoc 코드서명까지 포함
- `src/main.rs:138` `augment_path_for_macos()` — Finder(`.app`)로 실행하면 PATH 가
  `/usr/bin:/bin:/usr/sbin:/sbin` 뿐이라 Homebrew 툴을 못 찾는 문제를 이미 보정.
  Intel Homebrew 경로(`/usr/local/bin`, `/usr/local/opt/mysql-client/bin`)도 포함되어 있다.
- `src/main.rs:10` `KO_REGULAR` / `KO_BOLD` 폰트 후보에 `AppleSDGothicNeo.ttc` 이미 포함 →
  별도 폰트 설치 없이 한글 렌더됨
- `src/store.rs` 는 `$HOME` 기반이라 macOS 에서 그대로 동작

### 크로스컴파일은 불가

eframe/egui 는 실질적으로 크로스컴파일이 안 된다(`build-macos.sh` 자체가 `uname != Darwin`
이면 거부한다). **그 인텔 맥북에서 직접 빌드해야 한다.** 리눅스에서 대신 만들어 줄 방법은 없다.

### 맥북에서 할 일

```bash
xcode-select --install
curl --proto '=https' -sSf https://sh.rustup.rs | sh
brew install rsync mysql-client
brew install hudochenkov/sshpass/sshpass   # sshpass 는 homebrew-core 에 없음
./build-macos.sh --dmg
```

### 실제 걸림돌 3가지 (여기가 핵심)

1. **rsync 버전** — 코드가 `--mkpath`(`ops.rs:588`, `ops.rs:1871`, `ops.rs:3114`, `ops.rs:3197`)와
   `--info=progress2`(`ops.rs:3589`, `ops.rs:3621`)를 쓴다. **둘 다 rsync 3.2.3+ 기능.**
   macOS 기본 rsync 는 2.6.9(구버전) 또는 최신 macOS 의 openrsync 라 **둘 다 지원하지 않는다.**
   → `brew install rsync` 필수.
2. **sshpass** — homebrew-core 에 없어 별도 tap 이나 소스 빌드가 필요하다.
   이게 없으면 앱의 거의 모든 기능이 동작하지 않는다(`ops.rs` 전반에서 `sshpass -e` 사용).
3. **Gatekeeper** — ad-hoc 서명이라 첫 실행 시 우클릭 → 열기, 또는
   `xattr -dr com.apple.quarantine "dist/Hostmover.app"`. 정식 배포하려면 Developer ID 필요.

### 권장 보완 (작은 작업)

시작 시 로컬 `sshpass` / `rsync 3.x` / `mysqldump` 유무를 점검해, 없으면 설치 명령을 안내하는
진단 기능. 지금은 의존 툴이 없으면 실행 도중에 원인을 알기 어려운 에러로 튄다.

### 확인해 둔 것 (문제 없음)

- macOS 기본 bash 는 3.2 지만, 로컬 실행 스크립트에 bash4 전용 문법
  (`declare -A`, `mapfile`, `${var,,}`)은 없다.
- `stat -c`, `sed -i`, `timeout` 같은 GNU 의존 명령은 **원격(리눅스) 스크립트 안**에만 있고
  로컬 실행부에는 없다.

---

## 2. 아이패드 — 네이티브 포팅은 원천적으로 불가능

"mac 기반 아이패드"라고 해도 Apple Silicon 이든 아니든 아이패드는 **iPadOS** 다.
그리고 iOS/iPadOS 는 앱이 **자식 프로세스를 띄우는 것 자체를 금지한다(fork/exec 차단).**

Hostmover 의 본질은 로컬에서 `ssh` / `rsync` / `mysqldump` / `sshpass` 를 실행하는 것이므로,
egui 를 iOS 로 빌드하는 데 성공하더라도 **아무 기능도 동작하지 않는다.**
App Store 심사나 개발자 계정 문제는 그 다음 이야기다.

### 대안 A — 리눅스에서 돌리고 아이패드는 화면만 (코드 변경 0, 추천)

리눅스 PC/서버에서 hostmover 를 띄우고 아이패드 사파리로 접속.

```bash
xpra start --start=/path/to/hostmover --bind-tcp=0.0.0.0:14500 --html=on
# 아이패드 사파리 → http://서버:14500
```

- 장점: 바로 됨. 백업 파일이 서버 디스크에 쌓여 대용량 이전에 오히려 유리.
- 단점: 터치로 쓰기엔 UI 가 조밀함.
- **보안**: 자격증명 입력 화면이 그대로 노출되므로 **반드시 SSH 터널이나 VPN 뒤에 둘 것.**
  공인 IP 직접 노출 금지.
- 대체 수단: x11vnc/TigerVNC + noVNC 조합도 동일하게 가능.

### 대안 B — 클라이언트/서버 분리 (제대로 된 방법, 큰 작업)

`ops.rs`(4368줄)를 헤드리스 데몬으로 빼고 HTTP API 를 붙인 뒤, egui 를 wasm 으로 빌드해
웹 UI 로 쓴다. 그러면 아이패드·맥북·윈도우가 전부 브라우저로 해결된다.

- 걸림돌: `ops.rs` 전체가 `Command` 실행과 `String` 반환을 UI 와 직접 주고받는 구조라
  인증·세션·진행률 스트리밍을 새로 설계해야 한다.
- 참고: egui 를 wasm 으로 빌드해도 wasm 안에서는 `Command` 실행이 불가하므로,
  **어차피 서버 API 가 필요하다.** "wasm 으로만 포팅" 같은 지름길은 없다.
- 규모: 며칠 단위.

### 대안 C — 아이패드는 조회/명령복사 전용 (추가 개발 없음)

이미 있는 **📋 명령어 보기** 기능으로 명령만 확인·복사하고,
아이패드에서는 Termius 같은 SSH 앱으로 붙어 붙여넣어 실행.

---

## 3. 진행한다면 순서

1. 인텔 맥북에서 `./build-macos.sh --dmg` 를 한 번 돌려 본다 — 남은 건 위 의존성 3가지뿐.
2. 아이패드는 대안 A(xpra/VNC)로 먼저 써 본다.
3. 아이패드 사용 빈도가 실제로 높아지면 그때 대안 B 를 검토한다.

바로 착수할 수 있는 작업 두 가지:
- 맥북 쪽: 로컬 의존성(sshpass / rsync 3.x / mysqldump) 점검·안내 기능 추가
- 아이패드 쪽: 대안 A 용 xpra 실행 스크립트 작성
