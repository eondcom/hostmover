# Hostmover

> A Linux desktop GUI for migrating web hosting (files + MySQL) between servers,
> wrapping `mysqldump` / `rsync` / `tar`-over-SSH. Built with Rust + egui/eframe.

호스팅(사이트) 이전을 위한 리눅스 데스크탑 GUI 툴. `mysqldump` / `rsync` 작업을
손으로 치지 않고 GUI에서 클릭으로 백업·복원한다. (egui / eframe, Rust)

## 데이터 구조

```
고객 (예: omg)
  └─ 도메인 (이전 단위, 예: chailow.com)
       ├─ ① 도메인 접속정보   : 관리 URL, ID, PW, 도메인(한글→퓨니코드), 네임서버 메모
       ├─ ② 현재 사이트(ASIS) : IP, FTP ID, FTP PW, DB ID, DB PW, DB Name, Path
       ├─ ③ 신규 사이트(TOBE) : IP, FTP ID, FTP PW, DB ID, DB PW, DB Name, Path
       └─ ④ CMS 접속정보       : 관리자 URL, ID, PW
```

## 기능

- **DB 백업**  : 현재 사이트 → 로컬 (`mysqldump | gzip`, SSH 경유)
- **DB 복원**  : 로컬 → 신규 사이트 (`gunzip | mysql`)
- **파일 백업**: 현재 사이트 → 로컬 (`rsync -az` pull, 실패 시 `tar`-over-ssh 폴백)
- **파일 복원**: 로컬 → 신규 사이트 (`rsync -az` push, 실패 시 `tar`-over-ssh 폴백)
- **묶음 이전** (모두 실행 직전 build, 한 단계 실패 시 즉시 중단):
  - **🚀 전체 이전** : DB백업 → 파일백업 → DB복원 → 파일복원
  - **📁 파일만 이전**: 파일백업 → 파일복원
  - **🗄 디비만 이전**: DB백업 → DB복원
  - ※ 디스크 공간이 부족하면 '파일만' 먼저 → 공간 확보 후 '디비만' 으로 나눠 진행
- **⚡ 직접 이전** (로컬 디스크 미사용): 현재 → 신규 서버로 바로 스트리밍.
  로컬/원본 디스크 공간이 부족할 때 유용.
  - DB: `ssh 현재 "mysqldump…" | ssh 신규 "mysql…"`
  - 파일: `ssh 현재 "tar czf - ." | ssh 신규 "tar xzf -"` (rsync 불필요)
  - 현재/신규 비번이 달라도 되도록 `SSHPASS`를 `HM_ASIS`/`HM_TOBE` env로 분리(argv 노출 없음)
- **🔌 접속 테스트**: 현재/신규 사이트 각각 SSH 로그인 성공 여부 + 원격 도구
  (mysqldump/rsync/tar) 가용성 확인
- **📋 명령어 보기**: 각 작업을 실행하지 않고 실제 쉘 명령만 확인/복사
- **퓨니코드 변환**: 한글 도메인 → `xn--` (원본과 변환값을 나란히 표시)
- **자동 저장**: 입력이 멈추면 약 0.6초 후 자동 저장(디바운스)
- **복사 버튼(⎘)**: 아이디·비번 등 각 칸 값을 원클릭 클립보드 복사
- **네임서버 변경**: 직접 못 하므로 등록사/네임서버 정보 기록만

> 복원(덮어쓰기)은 확인 모달을 거친다. rsync는 사고 방지를 위해 `--delete`를 쓰지 않는다.

## 보안

- 모든 자격증명은 마스터 패스워드로 **로컬 암호화** 저장
  (Argon2id 키 유도 + AES-256-GCM). 파일: `~/.config/hostmover/store.enc`
- SSH/DB 비밀번호는 명령행(argv) 노출을 피하려고 `sshpass -e`(SSHPASS 환경변수)로 전달
- 백업 파일 보관: `~/.local/share/hostmover/backups/<고객>/<도메인>/`

## 전제 / 의존 CLI

로컬: `ssh`, `sshpass`, `rsync`, `tar`, `mysqldump`, `mysql`, `gzip`.
원격(호스팅): `mysqldump`/`mysql` + (`rsync` 또는 `tar`).
원격에 rsync가 없으면 파일 전송은 자동으로 `tar`-over-ssh로 폴백한다
(공유호스팅에서 `sh: rsync: not found` 대응).

> 입력은 FTP id/pw로 받지만 백업은 SSH(mysqldump/rsync)로 수행한다.
> 카페24·가비아 등 대부분 FTP 계정 = SSH 계정이라 그대로 SSH 로그인에 쓴다.
> SSH가 없는 순수 FTP-only 호스팅은 아직 미지원(추후 lftp 모드 예정).

## 빌드 / 실행

### Linux

```bash
cargo build --release
./target/release/hostmover
```

### macOS

> egui 앱은 사실상 크로스컴파일이 안 되므로 **macOS 에서 직접 빌드**해야 한다.

```bash
# 1) 의존 CLI 설치 (Homebrew)
brew install sshpass rsync mysql-client
#   ※ sshpass 는 정식 brew 포뮬러가 없을 수 있다:
#      brew install hudochenkov/sshpass/sshpass

# 2) 빌드 & 실행 (개발용)
cargo run --release

# 3) 배포용 .app 번들 만들기
./build-macos.sh            # 현재 아키텍처용 dist/Hostmover.app
./build-macos.sh --universal --dmg   # 유니버설(arm64+x86_64) + dist/Hostmover.dmg
open dist/Hostmover.app
```

처음 실행 시 Gatekeeper 가 막으면 **우클릭 → 열기**, 또는
`xattr -dr com.apple.quarantine dist/Hostmover.app`.

macOS 메모:
- `.app` 을 Finder 에서 실행하면 PATH 에 Homebrew 경로가 없어 `sshpass`/`rsync`/
  `mysqldump` 를 못 찾을 수 있다. 앱이 시작 시 `/opt/homebrew/bin`·`/usr/local/bin`
  등을 PATH 앞에 자동 보강하므로 일반적으로 그대로 동작한다.
- 한글 폰트는 macOS 기본 내장 **AppleSDGothicNeo** 를 자동 로드한다(별도 설치 불필요).
- 시스템 rsync(2.6.9)는 `--mkpath`/`--info=stats1` 미지원이라 **Homebrew rsync(3.x)**
  를 쓴다(위 PATH 보강으로 우선 적용됨).
- 저장 경로는 Linux 와 동일: 설정 `~/.config/hostmover/`, 백업 `~/.local/share/hostmover/`.

## 테스트

```bash
cargo test   # 암호화 왕복, 명령 생성/이스케이프, 검증, 퓨니코드
```

## 폰트 / 한글 입력

- UI 폰트는 `NanumGothicBold` 등 시스템 폰트를 자동 로드한다.
- egui는 0.29.x에서 리눅스 IME(한글 입력) 회귀 버그가 있어 **0.31.1로 고정**했다.
  fcitx5/ibus 환경에서 입력창 포커스 후 한글 입력이 동작한다.

## 보안 / 주의 (Security)

- **자격증명은 로컬 암호화 저장**: 마스터 패스워드로 Argon2id 키 유도 + AES-256-GCM.
  파일은 `~/.config/hostmover/store.enc` 에 저장되며 **저장소(repo)에 포함되지 않는다.**
- SSH/DB 비밀번호는 명령행(argv) 노출을 피하려 `sshpass -e`(SSHPASS 환경변수)로 전달한다.
- ⚠️ 편의를 위해 SSH 연결에 `StrictHostKeyChecking=no` 를 사용한다. 호스트 키 검증을
  생략하므로 **신뢰할 수 없는 네트워크에서는 중간자(MITM) 위험**이 있다. 보안이 중요한
  환경에서는 사전에 `known_hosts` 에 호스트 키를 등록해 쓰는 것을 권장한다.
- 이 도구는 **본인이 권한을 가진 서버의 이전 작업**을 위한 것이다. 타인 서버에 대한
  무단 접근에 사용하지 말 것.
- DB 복원/직접 이전은 대상(신규) 서버를 덮어쓴다. 실행 전 확인 모달을 거치며,
  rsync 는 사고 방지를 위해 `--delete` 를 쓰지 않는다.

## 라이선스 (License)

MIT — [LICENSE](LICENSE) 참고. © 2026 정낙훈 (eond)
