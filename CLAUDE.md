# hostmover 작업 규칙

## 코드를 고쳤으면 `./install-linux.sh` 로 끝낸다

`cargo build --release` 만 하고 "완료" 하지 말 것. 그건 런처(앱 아이콘·**하단 독바**)에 반영되지
않는다. 사용자가 실제로 실행하는 것은 `~/.local/bin/hostmover` 다.

```bash
./install-linux.sh            # 빌드 + 설치 + 런처/독바 갱신 + 버전 확인
./install-linux.sh --no-build # 이미 빌드했을 때
```

**worktree 안에서 실행해도 된다** — 설치 경로는 항상 `~/.local/bin` 이다.

끝나고 사용자에게 **"앱을 껐다 켜라"** 고 알린다. 실행 중인 프로세스는 옛 바이너리를 물고 있다.

### 반영됐는지 확인하는 법

앱 상단바·첫 화면·로그 첫 줄의 `v0.1.0 (커밋 · 날짜)` 가 유일한 근거다.
사용자가 보고한 해시가 방금 설치한 커밋과 다르면 **반영이 안 된 것이다.** 이때 확인할 것:

```bash
pgrep -af hostmover        # 어느 경로의 바이너리가 떠 있나
ps -p <PID> -o lstart      # 설치 시각 이후에 떴는데도 옛 버전이면 독바 캐시(§3-4)
```

COSMIC 독바는 `.desktop` 의 `Exec` 을 캐시해서, 고친 뒤에도 한동안 옛 경로로 앱을 띄운다.
`install-linux.sh` 가 옛 경로에도 새 바이너리를 얹어 이 문제를 덮는다.

자세한 배경과 실제 사고 기록: **[docs/updating.md](docs/updating.md)**

## 검증

- 코드를 바꿨으면 `cargo test` 를 돌리고 결과를 그대로 보고한다. 깨졌으면 깨졌다고 말한다.
- GUI 창을 띄워 확인하지 말 것. 기동 확인은 `timeout 8 ~/.local/bin/hostmover` 로 하고,
  종료코드 124(타이머까지 생존) + 출력 없음을 근거로 삼는다.

## 자격증명

DB 비번·SSH 비번·API 해시를 소스·테스트·스크립트에 박지 않는다.
`Job` 구조체는 비밀번호를 담고 있으므로 `#[derive(Debug)]` 를 붙이지 않는다
(테스트에서 `unwrap_err()` 대신 `.err().expect(...)` 를 쓰는 이유).
