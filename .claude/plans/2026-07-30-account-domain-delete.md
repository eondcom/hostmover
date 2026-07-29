# 계정/도메인 삭제 기능 — 계획

작성 2026-07-30. **구현 전 검토용 문서.** 아직 코드는 없다.

요구: 사이드바나 계정 관리에서 유저/도메인을 골라 삭제하면 **파일·DB·HestiaCP 등록까지**
정리되게. HestiaCP 쪽 도메인 제거가 가능한지 판단.

---

## 1. 결론 먼저

| 질문 | 답 |
|---|---|
| 파일 + DB 삭제 가능? | **가능.** DB 이름은 이미 `CMS_DETECT` 가 CMS별로 찾아낸다 |
| HestiaCP 에서 도메인도 없어지게 가능? | **가능하고, 오히려 이게 정석이다.** 어렵지 않다 |
| 계정 통째로 삭제 가능? | **가능.** `v-delete-user` 한 방에 도메인·DB·메일·크론·파일 전부 |

**중요한 건 "가능한가"가 아니라 "잘못 눌렀을 때 어떻게 되는가"다.** 이 기능은 되돌릴 수
없다. 그래서 아래 설계의 절반이 안전장치다.

### 파일을 직접 지우면 안 된다

`rm -rf /home/rokmc/web/example.com` 같은 직접 삭제는 하면 안 된다. HestiaCP 는 자기
설정(`/usr/local/hestia/data/users/<user>/web.conf`)과 nginx/apache 설정을 따로 들고 있어서,
파일만 지우면 **패널에는 도메인이 계속 남고 웹서버 설정도 남는다.** 유령 항목이 되고
나중에 같은 도메인을 다시 추가할 때 충돌한다.

반드시 HestiaCP CLI 를 경유한다. 그게 파일·DB·설정·nginx 재생성까지 한 번에 처리한다.

---

## 2. 쓸 명령 (HestiaCP CLI)

`/usr/local/hestia/bin/` 아래. 이미 `v-add-*`, `v-list-*` 는 이 프로젝트에서 쓰고 있다.

**조사 (읽기 전용)**

```bash
v-list-web-domains  <user> json     # 웹 도메인 목록
v-list-databases    <user> json     # DB 목록 (+ 크기)
v-list-dns-domains  <user> json
v-list-mail-domains <user> json
v-list-user         <user> json     # 디스크 사용량, 도메인 수
```

**삭제**

```bash
v-delete-web-domain  <user> <domain>     # 파일·설정·nginx/apache·SSL 까지
v-delete-dns-domain  <user> <domain>
v-delete-mail-domain <user> <domain>     # 메일함 포함
v-delete-database    <user> <db>         # DB + DB유저
v-delete-user        <user>              # 위 전부 + /home/<user> + 크론
```

**백업 (삭제 전 필수)**

```bash
v-backup-user <user>       # /backup/<user>.<날짜>.tar — 메일·크론·DNS까지 포함
```

`v-backup-user` 가 이 프로젝트의 `build_local_backup`(웹루트+DB덤프 tar.gz)보다 완전하다.
**계정 삭제 전에는 이걸 쓴다.** 도메인 단위 삭제는 로컬 백업으로 충분하다.

---

## 3. 두 종류의 "삭제"를 절대 섞지 말 것

지금 사이드바에는 **이미 삭제가 있다.** `app.rs` 의 "고객 삭제(휴지통)" / "도메인 삭제" —
이건 **hostmover 로컬 기록만** 휴지통으로 옮긴다. 서버는 건드리지 않는다.

여기에 서버 실삭제를 같은 모양으로 붙이면 사고가 난다. 사용자가 "도메인 삭제"를 눌렀을 때
그게 메모 정리인지 서버 파괴인지 구분이 안 되면, 언젠가 반드시 잘못 누른다.

| | 위치 | 하는 일 | 되돌리기 |
|---|---|---|---|
| **로컬 기록 삭제** (기존) | 사이드바 🗑 | hostmover 기록만 휴지통 | 휴지통에서 복구 |
| **서버 실삭제** (신규) | 계정 관리 페이지 | 서버의 파일·DB·HestiaCP 등록 | **불가** (백업 복원만) |

원칙:

- 서버 실삭제는 **사이드바에 두지 않는다.** 계정 관리 페이지 안에만 둔다.
  (사이드바는 "내 메모장", 계정 관리는 "실제 서버"라는 경계를 유지)
- 버튼 문구를 다르게 한다: 휴지통은 `삭제`, 서버는 **`서버에서 완전 삭제`**
- 서버 삭제 버튼은 빨간색 + 경고 아이콘. 확인 모달 제목에 서버 IP/호스트명을 박는다.

---

## 4. 안전장치 (이 기능의 핵심)

### 4-1. 3단계로 나눈다

한 번의 클릭으로 지워지지 않게 **점검 → 백업 → 삭제**를 분리한다.

1. **점검 (읽기 전용)** — 무엇이 지워지는지 먼저 보여준다.
   도메인, 웹루트 크기, DB 이름·크기, DNS/메일 도메인 유무, SSL, 그리고 아래 4-3 의 공유 검사.
   이 단계는 확인 모달 없이 바로 실행(아무것도 안 바꾸므로).
2. **백업** — 도메인 단위는 로컬 tar.gz, 계정 단위는 `v-backup-user`.
   **백업이 실패하면 삭제로 넘어가지 않는다.** 성공 시 백업 경로·크기를 로그에 남긴다.
3. **삭제** — 확인 모달 + 아래 4-2 의 타이핑 확인.

### 4-2. 이름을 직접 입력하게 한다

기존 `eond_confirm` 모달(예/아니오)만으로는 부족하다. 계정 삭제는 **계정명을 타이핑**해야
버튼이 활성화되게 한다. 도메인 삭제는 도메인명.

```
⚠ 서버에서 완전 삭제 — mars.eond.com

  rokmc / example.com
  파일 2.4GB · DB rokmc_wp (180MB) · DNS 있음 · 메일 없음

  되돌릴 수 없습니다. 백업: ~/.local/share/hostmover/backups/... (2.6GB, 완료)

  계속하려면 도메인을 입력하세요:  [              ]   [ 취소 ] [ 삭제 ]
```

### 4-3. DB 공유 검사 — 가장 위험한 함정

**한 DB를 여러 도메인이 쓰고 있으면 도메인 하나 지우려다 다른 사이트를 죽인다.**
개발/스테이징 사이트가 운영 DB를 그대로 참조하는 경우가 실제로 흔하다.

삭제 대상 DB 이름을 그 계정의 **모든 웹루트 설정파일에서 역검색**한다:

```bash
# wp-config.php / db.config.php / dbconfig.php / .env 전부에서 DB 이름 등장 횟수
grep -rl "$DB" /home/$USER/web/*/public_html/{wp-config.php,data/dbconfig.php} \
                /home/$USER/web/*/public_html/{files/,}config/db.config.php \
                /home/$USER/web/*/pythonapp/.env 2>/dev/null
```

2곳 이상에서 참조되면 **DB 삭제를 건너뛰고** 그 사실을 보고한다(파일/도메인만 삭제).
사용자가 그래도 지우겠다면 별도 확인을 한 번 더 받는다.

### 4-4. 보호 대상 하드코딩

다음은 **입력 검증 단계에서 거부**한다. 실수로도 지워지면 서버가 죽는다.

- `admin` (HestiaCP 관리자)
- 설정의 서버 SSH 계정 자신 (`Settings.ssh_user`) — 자기 발등 찍기 방지
- `root`, `www-data`, `mysql`, `hestiaweb` 등 시스템 계정
- `v-list-users` 결과에 없는 계정 (= HestiaCP 계정이 아님 → 손대지 않는다)

기존 `is_safe_name` / `to_ascii_domain` 검증은 그대로 재사용한다(주입 방지).

### 4-5. 삭제 후 검증 + 기록

삭제가 "됐다고 하는데 안 된" 경우를 잡는다.

```bash
v-list-web-domains "$USER" | grep -q "$DOM" && echo "✗ 패널에 아직 남아 있음"
[ -d "/home/$USER/web/$DOM" ] && echo "✗ 디렉터리 잔존"
mysql -N -e "SHOW DATABASES LIKE '$DB'" | grep -q . && echo "✗ DB 잔존"
```

그리고 서버에 삭제 이력을 남긴다 — 나중에 "누가 이거 지웠지?"를 답할 수 있어야 한다.

```
/var/log/hostmover/deletions.log
2026-07-30 14:22:01  DELETE domain rokmc/example.com  files=2.4GB db=rokmc_wp(180MB) backup=OK
```

---

## 5. 구현 계획

### ops.rs — 새 함수 5개

기존 `build_account_modules_delete` 의 검증·조립 패턴을 그대로 따른다.

| 함수 | 성격 | 하는 일 |
|---|---|---|
| `build_domain_delete_probe(s, acct, domain)` | 읽기 | 지워질 것 목록 + 크기 + DB 공유 검사 |
| `build_domain_delete(s, acct, domain, drop_db)` | **파괴** | `v-delete-web-domain` → DNS → 메일 → (DB) → 검증 |
| `build_account_delete_probe(s, acct)` | 읽기 | 계정의 전체 도메인·DB·용량 |
| `build_account_backup(s, acct)` | 서버변경(안전) | `v-backup-user` |
| `build_account_delete(s, acct)` | **파괴** | `v-delete-user` → 검증 |

`drop_db` 는 4-3 검사 결과에 따라 UI 에서 정해 넘긴다.

DB 삭제는 `v-delete-database` 우선, 실패 시(패널에 등록 안 된 수동 생성 DB) `DROP DATABASE`
폴백 — 단 이 폴백은 사용자 확인을 받은 경우만.

### app.rs — UI

계정 관리 페이지(`AcctTab`)에 **`위험 작업` 탭 신규**. 사이트/모듈 탭과 분리해서, 실수로
들어가지 않게 한다.

- 사이트 탭의 `sel` 체크박스(이미 있음)로 도메인 선택 → 위험 작업 탭에서 대상 확인
- `점검` → `백업` → `서버에서 완전 삭제` 3버튼 순차 (앞 단계 미완료면 다음 버튼 비활성)
- 계정 전체 삭제는 이 탭 맨 아래, 접힌 섹션(`CollapsingHeader`)으로 한 번 더 숨긴다
- 삭제 성공 후 사이드바 로컬 기록도 지울지 **따로 묻는다** (자동 연동하지 않는다 — 3장의 경계 유지)

### 테스트

`disk_monitor_jobs_valid_bash` 와 같은 방식으로:

- 생성 스크립트 `bash -n` 문법 검증
- 보호 계정(`admin`, `root`, ssh_user 자신) 거부 확인
- 주입 시도 거부 (`../etc`, 따옴표 포함 도메인)
- `drop_db=false` 일 때 스크립트에 `v-delete-database` 가 **없어야** 함
- shim(`v-*` 가짜 명령)으로 삭제 순서·검증 단계 실제 실행

---

## 6. 단계별 진행 제안

한 번에 다 만들지 말고 끊는다. **읽기 기능만 먼저 넣어 쓰면서 신뢰가 생긴 뒤** 파괴 기능을 붙인다.

| 단계 | 내용 | 위험 |
|---|---|---|
| 1 | 점검(probe) 2개 + UI 표시만 | 없음 (읽기 전용) |
| 2 | 백업(`v-backup-user`) + 백업 목록 확인 | 낮음 |
| 3 | **도메인** 삭제 (DB 공유 검사 포함) | 높음 |
| 4 | **계정** 삭제 | 매우 높음 |

3단계 전에 테스트 계정(`hmtest` + 더미 도메인)을 서버에 만들어 실제로 지워보고, 4-5 의
검증이 실제로 통과하는지 확인한 뒤 실전에 쓰는 것을 권한다.

---

## 7. 미확정 — 결정 필요

1. **삭제 후 사이드바 로컬 기록**: 자동 휴지통 이동 vs 매번 묻기.
   → 지금은 "매번 묻기"로 계획했다. 자동이 편하지만 경계가 흐려진다.
2. **백업 강제 여부**: 백업 실패 시 삭제를 무조건 막을지, 사용자가 무시할 수 있게 할지.
   → "무조건 막기"로 계획했다. 무시 옵션은 결국 눌리게 된다.
3. **`/backup` 용량**: `v-backup-user` 는 계정 크기만큼 쓴다. 디스크가 꽉 차 있으면 실패한다.
   → 백업 전 `df` 확인을 넣는다. (디스크 점검 기능과 연계)
4. 여러 도메인 **일괄 삭제**를 허용할지. → 1차에서는 **한 번에 하나만**. 일괄은 사고 규모를 키운다.

---

## 8. 관련

- 매일 디스크 점검(백업 공간 확인과 연계): [../../docs/disk-check.md](../../docs/disk-check.md)
- 일괄 업데이트(같은 계정/도메인 순회 패턴): [../../docs/bulk-update.md](../../docs/bulk-update.md)
