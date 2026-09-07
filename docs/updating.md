# 업데이트 / 배포 (Linux)

코드를 고친 뒤 **런처에서 실행되는 앱까지 실제로 바뀌게** 하는 절차. 그리고 지금 돌고 있는
바이너리가 어느 버전인지 확인하는 법.

---

## 1. 지금 실행 중인 버전 확인

앱 세 곳에 같은 문자열이 나온다.

| 위치 | 언제 보이나 |
|---|---|
| 잠금화면(첫 실행화면) 부제 아래 | 앱을 켜자마자 |
| 상단바 "Hostmover" 옆 | 잠금 해제 후 항상 |
| 로그창 첫 줄 | 진단 결과를 복사해 붙여넣을 때 같이 딸려온다 |

```
v0.1.0  (21c4383 · 2026-07-25 07:36)
        └ git 커밋   └ 그 커밋의 날짜
```

- `+수정본` 이 붙으면(`21c4383+수정본`) **커밋되지 않은 변경이 섞인 빌드**다.
- `nogit` 이면 git 정보 없이 빌드된 것(소스 tarball 등).

이 값은 `build.rs` 가 빌드 시점에 `git rev-parse` / `git log` 로 읽어 실행파일에 새긴다.
빌드 시각이 아니라 **커밋 시각**을 쓰므로, 같은 소스는 몇 번을 빌드해도 같은 값이 나온다.

---

## 2. 표준 업데이트 절차

```bash
cd ~/dev/hostmover
git pull                      # 또는 브랜치 머지
cargo build --release
cp -a ~/.local/bin/hostmover ~/.local/bin/hostmover.bak-$(date +%Y%m%d)   # 되돌리기용
install -m755 target/release/hostmover ~/.local/bin/hostmover
```

런처(`~/.local/share/applications/hostmover.desktop`)의 `Exec` 은
**`~/.local/bin/hostmover`** 를 가리킨다(2026-08-25 부터. 그 전엔 `target/release/` 직접 참조).
빌드만으로는 런처가 안 바뀌고, **마지막 `install` 줄까지 해야 반영된다.**
앱이 떠 있으면 껐다 켜야 한다.

반영됐는지는 앱을 켜서 §1 의 버전 문자열이 바뀌었는지로 확인한다.

---

## 3. 함정 — "고쳤는데 런처에서 안 보인다"

가장 흔한 사고이고, 2026-07-25 에 실제로 겪었다.

### 3-1. worktree 에서 빌드했다

`.claude/worktrees/<이름>/` 에서 작업하면 빌드 산출물도 **그 worktree 안**에 생긴다.

```
.claude/worktrees/php-fpm-log-viewer/target/release/hostmover   ← 새 기능 있음
~/dev/hostmover/target/release/hostmover                        ← 런처가 실행하는 것 (옛것)
```

런처는 원본 경로만 보므로 아무리 빌드해도 화면이 그대로다. 해결:

```bash
# (권장) 브랜치를 머지한 뒤 원본에서 빌드 → 설치
cd ~/dev/hostmover && git merge <브랜치> && cargo build --release \
  && install -m755 target/release/hostmover ~/.local/bin/hostmover

# (급할 때) worktree 빌드 산출물을 바로 설치 — 소스(원본 체크아웃)와 어긋나므로 임시 조치로만
cp -a ~/.local/bin/hostmover ~/.local/bin/hostmover.bak-$(date +%Y%m%d)
install -m755 .claude/worktrees/<이름>/target/release/hostmover ~/.local/bin/hostmover
```

바이너리만 얹으면 소스와 실행파일이 어긋난다. 버전 문자열에 그 커밋 해시가 그대로 찍히므로
나중에 추적은 되지만, 되도록 머지 후 정식 빌드를 한다.

### 3-2. 디버그 빌드만 했다 / 설치를 안 했다

`cargo build` 는 `target/debug/` 에, `cargo build --release` 는 `target/release/` 에 만든다.
그리고 런처는 `target/` 을 보지 않고 **`~/.local/bin/hostmover` 만** 본다. release 빌드 뒤
`install` 로 복사하는 단계를 빼먹으면 화면이 그대로다.

### 3-3. 앱을 안 껐다

실행 중인 프로세스는 옛 바이너리를 그대로 물고 있다. 껐다 켠다.

---

## 4. 런처 항목 (.desktop)

`~/.local/share/applications/hostmover.desktop`

```ini
[Desktop Entry]
Type=Application
Name=Hostmover
Exec=/home/dell/.local/bin/hostmover
Icon=hostmover
Terminal=false
Categories=Network;
StartupWMClass=hostmover
```

- 아이콘 원본은 `assets/hostmover.png`.
- 파일을 고쳤는데 메뉴에 안 뜨면 `update-desktop-database ~/.local/share/applications` 를 돌린다.
- 항목 자체가 사라진 게 아니라 **Exec 이 옛 바이너리를 가리키는 것**이 대부분이다(§3).

---

## 5. macOS

`build-macos.sh` 가 `.app` 번들을 만든다. 자세한 내용은 README 의 "빌드 / 실행 > macOS".
egui 앱은 사실상 크로스컴파일이 안 되므로 macOS 에서 직접 빌드해야 한다.

---

## 6. 되돌리기

```bash
# 설치본만 되돌릴 때 (install 전에 만든 백업으로)
mv ~/.local/bin/hostmover.bak-YYYYMMDD ~/.local/bin/hostmover

# 소스까지 되돌릴 때
cd ~/dev/hostmover && git checkout <이전커밋> && cargo build --release
```
