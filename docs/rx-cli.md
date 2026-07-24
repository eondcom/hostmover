# rx-cli — Rhymix 터미널 도구

wp-cli 의 Rhymix 판. 직접 만든 도구이고, hostmover 에서 서버로 배포할 수 있다.

---

## 1. 소스 위치

```
/home/dell/dev/rx/modules/rxdashboard/bin/rx
```

rxdashboard 모듈(Rhymix 대시보드)에 딸린 단일 PHP 파일이다. 의존성 없이 이 파일 하나면 동작한다.

hostmover 는 **설정 > Rhymix 소스** 경로(`rx_source_local`) 하위의
`modules/rxdashboard/bin/rx` 를 읽는다. 그 설정이 `/home/dell/dev/rx` 를 가리키면 위 경로가 된다.

---

## 2. 배포

**hostmover**: 설정 > 일괄 업데이트 탭 > "rx-cli 배포" 카드 > `rx-cli 배포` 버튼.

하는 일:

1. 기존 `/usr/local/bin/rx` 가 있으면 `rx.bak.<타임스탬프>` 로 백업
2. 파일 내용을 그대로 `/usr/local/bin/rx` 에 쓰고 `755 root:root`
3. `php -l` 로 문법 검사 (실패하면 즉시 중단)
4. 서버의 Rhymix 사이트를 하나 찾아 `rx version` 을 시험 실행

수동으로 하려면:

```bash
scp /home/dell/dev/rx/modules/rxdashboard/bin/rx  서버:/tmp/rx
ssh 서버 'sudo install -m755 -o root -g root /tmp/rx /usr/local/bin/rx && php -l /usr/local/bin/rx'
```

### 심볼릭 링크 대신 복사인 이유

파일 머리말에는 `ln -s` 로 심볼릭 링크를 걸라고 적혀 있는데, 그건 **개발 PC 기준**이다.
서버에는 Rhymix 사이트가 수십 개라 특정 사이트의 rxdashboard 를 가리키게 하면 그 사이트를
지우는 순간 rx 가 깨진다. 그래서 파일 자체를 복사한다.

`rx` 는 실행 위치를 이렇게 찾는다:

1. 스크립트 위치 기준 3단계 위(`modules/rxdashboard/bin/rx` → Rhymix 루트) — 복사본에서는 실패
2. **현재 작업 디렉터리에서 위로 올라가며** `index.php` + `common/` 이 있는 곳 — 이걸로 동작

즉 복사해 두면 wp-cli 처럼 아무 Rhymix 사이트에서나 쓸 수 있다.

---

## 3. 사용

**`--path` 옵션이 없다.** 반드시 사이트 디렉터리 안에서 실행한다.

```bash
cd /home/<유저>/web/<도메인>/public_html
rx version
```

| 명령 | 하는 일 |
|---|---|
| `rx version` | 코어/PHP 버전 |
| `rx cache flush` | 전체 캐시 비우기 |
| `rx module list` | 설치된 모듈 목록 |
| `rx module install <zip\|dir>` | 모듈 설치 |
| `rx module delete <name>` | 모듈 삭제 |
| `rx update check` | 코어 최신 버전 확인 |
| `rx db cli` | 설정된 DB 로 mysql 접속 |
| `rx help` | 도움말 |

사이트 소유자 권한으로 돌려야 파일 소유권이 어긋나지 않는다:

```bash
cd /home/eond/web/eond.com/public_html
sudo -u eond rx cache flush
```

---

## 4. 주의

- Rhymix 루트를 못 찾으면 `Rhymix 루트를 찾을 수 없습니다` 로 종료한다. `cd` 를 확인할 것.
- `rx db cli` 는 사이트 설정의 DB 자격증명으로 접속하므로, 공용 터미널에서 쓰지 말 것.
- root 로 실행하면 캐시 파일이 root 소유로 생겨 사이트가 깨질 수 있다. `sudo -u <유저>` 를 쓴다.
- 업데이트하려면 소스를 고친 뒤 hostmover 에서 다시 배포하면 된다(기존 파일은 자동 백업).

---

## 5. 관련

- 일괄 업데이트 정책: [bulk-update.md](bulk-update.md)
- hostmover 자체 업데이트: [updating.md](updating.md)
