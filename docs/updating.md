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

## 2. 표준 업데이트 절차 — `./install-linux.sh` 하나로 끝낸다

```bash
./install-linux.sh            # 릴리스 빌드 + 설치 + 런처/독바 갱신
./install-linux.sh --no-build # 이미 빌드했을 때
```

**worktree 안에서 실행해도 된다.** 오히려 그게 정상 경로다 — 빌드 산출물은 worktree 안에
생기지만 설치는 항상 `~/.local/bin/hostmover` 로 간다.

스크립트가 하는 일:

| 단계 | 내용 |
|---|---|
| ① | `cargo build --release` (런처는 debug 를 보지 않는다) |
| ② | 바이너리를 `~/.local/bin/hostmover` 로 **복사** — `cargo clean` 에 안 날아간다 |
| ③ | 아이콘을 `~/.local/share/icons/hicolor/256x256/apps/` 로 |
| ④ | `.desktop` 을 다시 쓰고 `update-desktop-database` |
| ⑤ | 옛 경로(`<메인체크아웃>/target/release/hostmover`)가 남아 있으면 **같이 덮는다** (§3-4 독바 캐시 대비) |
| ⑥ | 설치된 바이너리의 커밋 해시를 찍고, 앱이 실행 중이면 껐다 켜라고 알린다 |

끝나면 앱을 껐다 켜고, §1 의 버전 문자열이 스크립트가 찍은 해시와 같은지 확인한다.

> **수동으로 할 때도 `Exec` 은 절대 `target/release` 를 직접 가리키지 말 것.**
> 빌드 산출물 경로라 `cargo clean` 한 번에 사라지고, 그러면 `Terminal=false` 라 에러 한 줄 없이
> 아이콘이 먹통이 된다(§3-0).

---

## 3. 함정 — "고쳤는데 런처에서 안 보인다"

가장 흔한 사고다. 2026-07-25, 2026-08-05 에 실제로 겪었다.
**아래 전부 `./install-linux.sh` 로 예방된다.** 수동으로 할 때만 신경 쓰면 된다.

### 3-0. release 바이너리가 아예 없다 (2026-08-05)

런처 `Exec` 이 `target/release/hostmover` 를 직접 가리키던 시절, `cargo clean`(또는 재체크아웃)
이후 릴리스 빌드를 안 하면 그 파일이 **존재하지 않는다.** `Terminal=false` 라서 아이콘을 눌러도
에러창 하나 없이 아무 일도 안 일어난다 — "앱이 실행이 안 된다" 로 보이는 전형적 증상이다.

```bash
ls -la ~/dev/hostmover/target/release/hostmover   # 없으면 이 경우
```

그래서 지금은 `Exec` 이 `~/.local/bin/hostmover` 를 가리킨다. `install` 은 복사이므로
`cargo clean` 을 해도 살아남는다.

### 3-1. worktree 에서 빌드했다

`.claude/worktrees/<이름>/` 에서 작업하면 빌드 산출물도 **그 worktree 안**에 생긴다.

```
.claude/worktrees/php-fpm-log-viewer/target/release/hostmover   ← 새 기능 있음
~/dev/hostmover/target/release/hostmover                        ← 런처가 실행하는 것 (옛것)
```

런처가 worktree 안을 볼 리 없으니 아무리 빌드해도 화면이 그대로다. 해결:

```bash
# worktree 안에서 그냥 이걸 실행하면 된다 (설치 경로는 항상 ~/.local/bin)
./install-linux.sh
```

이때 실행파일은 worktree 커밋으로 만들어지고 메인 체크아웃의 소스와는 어긋난다. 버전 문자열에
그 커밋 해시가 찍히므로 추적은 되지만, 검증이 끝나면 머지하고 다시 설치한다:

```bash
cd ~/dev/hostmover && git merge <브랜치> && ./install-linux.sh
```

### 3-2. 디버그 빌드만 했다

`cargo build` 는 `target/debug/` 에, `cargo build --release` 는 `target/release/` 에 만든다.
런처는 **release 만** 본다.

### 3-3. 앱을 안 껐다

실행 중인 프로세스는 옛 바이너리를 그대로 물고 있다. 껐다 켠다.
어느 파일을 물고 있는지는 이렇게 본다:

```bash
pgrep -af hostmover        # 실행 중인 프로세스의 실제 경로
ps -p <PID> -o lstart      # 언제 떴는지 (설치 시각보다 이전이면 그냥 옛 창이다)
```

### 3-4. 독바(COSMIC)가 옛 `Exec` 을 캐시하고 있다 (2026-08-05)

이 데스크톱은 COSMIC 이고, 하단 독바 즐겨찾기는
`~/.config/cosmic/com.system76.CosmicAppList/v1/favorites` 에 **desktop ID**(`"hostmover"`)로만
들어 있다. 즉 경로가 아니라 `.desktop` 파일을 따라간다 — 그래서 `.desktop` 만 고치면 될 것 같지만,
**COSMIC AppList 가 `Exec` 값을 캐시한다.** `.desktop` 을 고치고 `update-desktop-database` 까지
돌려도 독바 아이콘은 한동안 옛 경로로 앱을 띄운다.

실제로 겪은 순서가 이랬다: `Exec` 을 `~/.local/bin` 으로 바꾸고 새 바이너리를 설치했는데,
독바로 띄운 앱이 계속 **옛 커밋 해시**를 보여줬다. 프로세스를 보니 옛 경로였다.

```bash
pgrep -af hostmover
# → /home/dell/dev/hostmover/target/release/hostmover   ← 캐시된 옛 Exec
```

대응은 둘 중 하나다:

1. **옛 경로에도 새 바이너리를 얹는다** — `install-linux.sh` 가 ⑤에서 자동으로 한다.
   캐시가 남아 있어도 실행 결과가 같아지므로 가장 확실하다.
2. 독바/패널을 재시작해 캐시를 비운다. (세션이 잠깐 깜빡인다)

---

## 4. 런처 항목 (.desktop)

`~/.local/share/applications/hostmover.desktop` — **`install-linux.sh` 가 매번 다시 쓴다.**
손으로 고칠 일은 거의 없다.

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

- **`Exec` 은 `~/.local/bin/hostmover`.** 빌드 산출물 경로를 직접 가리키지 않는다(§3-0).
- 아이콘 원본은 `assets/hostmover.png` → `~/.local/share/icons/hicolor/256x256/apps/hostmover.png`.
- 파일을 고쳤는데 메뉴에 안 뜨면 `update-desktop-database ~/.local/share/applications`.
- 항목이 사라진 게 아니라 **Exec 이 옛 바이너리를 가리키거나(§3-0) 독바가 캐시 중(§3-4)** 인 경우가 대부분이다.

---

## 5. macOS

`build-macos.sh` 가 `.app` 번들을 만든다. 자세한 내용은 README 의 "빌드 / 실행 > macOS".
egui 앱은 사실상 크로스컴파일이 안 되므로 macOS 에서 직접 빌드해야 한다.

---

## 6. 되돌리기

```bash
# 소스를 되돌리고 다시 설치
cd ~/dev/hostmover && git checkout <이전커밋> && ./install-linux.sh
```

설치가 복사(`install`)라서 되돌리기도 "옛 커밋으로 다시 설치" 한 번이면 된다.
별도 `.bak` 을 두지 않는다 — 어느 바이너리가 어느 커밋인지는 §1 의 버전 문자열로 항상 확인된다.
