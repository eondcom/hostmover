# eondcms 설치 통합 계획 (hostmover)

hostmover에 **eondcms 신규 인스턴스 설치(HestiaCP 멀티테넌트)** 기능을 통합하기 위한 계획.
방식은 **설치 스크립트 생성기 + HestiaCP `v-*` CLI 자동화**.

> 원본 설치 절차: `~/dev/eondcms/docs/hestiacp-new-tenant-install.md` (13단계).
> 이 문서는 그 절차를 hostmover에 얹기 위한 **통합 설계서(canonical)**다.
> (구 메모 `eondcms-install-plan.md`의 내용도 여기로 흡수했다.)

## 배경 — 왜 eondcms 설치를 hostmover에?

- eondcms의 "호스트 생성 마법사"는 **eondcms 운영 서버가 이미 떠 있어야** HestiaCP API로 사이트를
  만든다. 그런데 목표는 *eondcms가 아직 없는 빈 새 서버*에 까는 것 → **닭-달걀 문제**.
- hostmover는 **SSH만 있으면 되는 독립 데스크탑 툴**이라 빈 서버 설치에 적합(원 요청도 hostmover).
- 방식은 eondcms 호스트 생성과 동일(도메인→DB→설치→SSL), **실행 주체만 hostmover**.

---

## 0. 어디서 작업하나

- **구현은 hostmover 세션(이 레포, `~/dev/hostmover`)에서 한다.** 만들 코드는 전부 hostmover(Rust):
  스크립트 생성기·UI·ops.
- eondcms 세션은 eondcms 자체(FastAPI/Svelte) 코드용이며, 그쪽 `CLAUDE.md` 규칙(피크시간 금지·
  haiku 서브에이전트·plans 경로 등)은 eondcms 작업에만 적용된다.
- eondcms의 설치 세부(nginx 템플릿 `.claude/hestiacp/*.tpl`, `.env` 키, alembic, `v-*` 인자)는
  **참조 자료로만** 가져온다(스크립트에 값이 박힐 뿐, eondcms 코드를 고치지 않는다).

---

## 1. 컨셉

도메인의 **설치 대상 사이트(ASIS 또는 TOBE 선택) SSH = 설치할 HestiaCP 서버**로 보고:

```
[폼 입력] → [멱등 bash 설치 스크립트 생성] → 📋 미리보기/복사  또는  🚀 루트로 원클릭 실행
```

기존 hostmover 인프라를 100% 재사용:
- SSH 실행(`sshpass`, `remote_cmd`의 PATH 보강, 로그 스트리밍)
- **루트 옵션**(`서버루트 ID/PW` + "루트로 실행") — `v-*`/systemd/nginx/SSL은 root 필요
- **📋 명령어 보기** 모달(미리보기/복사), **확인 모달**, **녹색/빨강 결과**, **자격증명 로컬 암호화 저장**
- `bash -n` 구문 검증 테스트

---

## 2. 데이터 모델 (`src/model.rs`)

`Domain`에 `eond: EondInstall` 추가:

| 필드 | 설명 | 기본값 |
|------|------|--------|
| `hestia_user` | HestiaCP 유저 (예: jokbo) | — |
| `hestia_pass` | `v-add-user`용 비밀번호 | — |
| `hestia_email` | `v-add-user`용 이메일 | — |
| `package` | HestiaCP 패키지 | `default` |
| `port` | uvicorn 포트(127.0.0.1, 인스턴스 고유) | — |
| `db_name` | DB 짧은 이름(HestiaCP가 `user_` 자동 접두) | — |
| `db_user` | DB 짧은 유저명(동일) | — |
| `db_pass` | DB 비밀번호 | — |
| `table_prefix` | Rhymix/XE 테이블 접두어 | `xe_` |
| `admin_user` | 관리자 ID | `admin` |
| `admin_pass` | 관리자 비밀번호 | — |
| `code_local` | rsync 소스(dev의 eondcms `pythonapp` 경로) | — |
| `code_src` | (대안) git URL — source만 clone 시 | — |
| `target` | 설치 대상 사이트 (asis/tobe 선택) | `tobe` |

> 코드 전송 기본은 **rsync**(§6.1 A): `code_local` → 대상 `$APPDIR`. `.venv` 제외, `web/build` 포함.
> 포트는 빈칸이면 설치 스크립트가 **8001부터 빈 포트 자동 탐색**(`ss -ltnp`)하도록 할 수 있다.
> 기존 `Site`의 `ip·root_id/pw·db_*·path` 는 최대한 재사용한다.

---

## 3. ops (`src/ops.rs`)

- `OpKind::EondInstall` + `build_eondcms(domain, &EondInstall, server: &Site, use_root) -> Job`
- nginx 템플릿은 **hostmover에 인라인 내장**(아래 §3.1). 도메인 무관·포트만 치환.
- 생성 스크립트 **단계(전부 멱등), 순서 중요**:
  1. **포트 충돌 확인** — `ss -ltnp | grep :<PORT>` 사용 중이면 중단(다른 포트 안내)
  2. `v-list-user || v-add-user` (HestiaCP 유저)
  3. `v-list-web-domain || v-add-web-domain`
  4. `v-list-database || v-add-database` (DB명/유저는 `user_` 접두어 자동)
  5. 코드: **dev→대상 rsync push**(별도 "코드 업로드" 단계, §6.1 A)로 `$APPDIR`에 올림.
     설치 스크립트는 `$APPDIR/app/main.py` + **`web/build/` 존재 확인**(없으면 정적 깨짐 경고/중단).
     (대안: source는 `git clone` + `web/build`만 rsync)
  6. `python3.11 -m venv` + `poetry install` (해당 유저로)
  7. `.env` 작성 — **이미 있으면 유지**(SECRET_KEY/FERNET_KEY 보존). 없으면
     `openssl rand -hex 32` / `Fernet.generate_key()`로 생성
  8. `alembic upgrade head`
  9. `systemd` 유닛(`eondcms-<유저>`, `--port <포트>`) 생성 + `enable --now` + uvicorn 헬스체크(`curl 127.0.0.1:<PORT>`)
  10. **`v-add-letsencrypt-domain` (SSL 먼저!)** — DNS A레코드가 이 서버를 가리킨 상태 필요
  11. nginx 템플릿 생성: 내장 템플릿의 `8001`→`<PORT>` 치환하여
      `eondcms-<PORT>.tpl`/`.stpl` 작성(644, root) → `v-change-web-domain-proxy-tpl <user> <domain> eondcms-<PORT>`
  12. 최종 확인 `curl -I https://<도메인>`
- ⚠️ **10(SSL) → 11(proxy-tpl) 순서 고정**: SSL 없이 `.stpl`(ssl_certificate 참조) 적용 시 nginx 기동 실패.
- 루트 필요 → 기존 "루트로 실행" 토글 사용. `remote_cmd`(PATH 보강)·`bash -n` 검증 재사용.
- **use_root 폴백**: root 계정이 있으면 1·2·3·9~12단계(포트확인·v-*·systemd·SSL·nginx) 자동 실행.
  root가 없으면(패널 유저=sudo 불가) 그 단계들은 **실행하지 않고 명령만 출력**해 수동 실행을 안내.
  (앱/venv/.env/alembic 등 유저 권한 단계만 자동)
- **코드 업로드 = rsync(dev→대상)** 기본(§6.1 A). hostmover의 기존 rsync push(+tar 폴백) 재사용.
  → 설치 op과 별개의 "코드 업로드" 액션으로 두거나, 설치 직전 단계로 묶음.

### 3.1 내장 nginx 템플릿 (포트만 가변)

`eondcms.tpl`/`eondcms.stpl` 원문을 hostmover 상수로 내장한다. 정적 경로는 모두
`%home%/%user%/web/%domain%/...` HestiaCP 플레이스홀더라 **도메인/유저 무관**하고,
유일한 인스턴스 변수는 `location / { proxy_pass http://127.0.0.1:8001; }` 의 **포트**다.
설치 시 `8001`을 `<PORT>`로 치환해 `eondcms-<PORT>.tpl/.stpl`로 기록한다.

- `.tpl`(HTTP): `listen %ip%:%proxy_port%`, 정적(`/static/ /_app/ /files/ /modules/ /layouts/
  /m.layouts/ /addons/ /widgets/ /widgetstyles/ /common/`) 직접 서빙 + 나머지 `proxy_pass`(WebSocket 헤더, `client_max_body_size 50M`)
- `.stpl`(HTTPS): 위와 동일 + `listen %ip%:%proxy_ssl_port% ssl; http2 on; ssl_certificate %ssl_pem%;`
- 원본: `~/dev/eondcms/.claude/hestiacp/eondcms.tpl`, `eondcms.stpl` (hostmover 빌드 시 그대로 복사 내장)

---

## 4. UI (`src/app.rs`)

- 도메인에 **"eondcms 설치 (HestiaCP)"** 접기 패널 + 입력 칸(복사 버튼 포함)
- 버튼 2개:
  - **`📋 설치 스크립트`** — 기존 cmd_view 모달 재사용(미리보기/복사)
  - **`🚀 eondcms 설치`** — 확인 모달 → 루트로 실행
- **포트 대장**: 저장된 모든 도메인의 `eond.port`를 모아 "사용 중 포트 / 다음 빈 포트 제안" 표시(충돌 방지)

---

## 5. 단계(Phase)

- **Phase 1 (1차 목표)**: "코드 업로드(rsync)" + 설치 스크립트 생성 + 📋 미리보기 + 🚀 실행(전체 단계).
- **Phase 2 (후속)**: nginx 템플릿 자동(인라인은 Phase 1에 포함), 포트 자동 할당,
  설치 후 자동 점검(헬스체크 → 실패 시 로그·자동 복구 루프).

---

## 6. 결정 사항 / 남은 질문

해결된 것:
- ✅ **nginx 템플릿 = 인라인 내장**(§3.1). 도메인 무관이라 포트만 치환 → `/tmp`·scp 불필요.
- ✅ **순서 = SSL → proxy-tpl** 고정(§3 경고).
- ✅ **HestiaCP 리소스 = `v-*` 자동**(존재 시 skip, 멱등). (사용자 결정)

### 6.1 web/build 전송 (결정 #1)

`web/build`(SvelteKit 산출물, **약 7.3MB / 289파일**)는 eondcms에서 `.gitignore` 처리되어
`git clone`만으론 프론트가 빠진다(서버엔 Node 미설치 전제). 게다가 빌드본은 **파일명이 매 빌드
해시로 바뀌어 git 델타 압축이 거의 안 먹혀**, 커밋하면(dist든 main이든) **빌드마다 ~7MB가 영구 누적**.

| 옵션 | 방식 | 평가 |
|------|------|------|
| **A. rsync (추천)** | dev에서 `npm run build` 후 hostmover가 `pythonapp/`(.venv 제외, web/build 포함)를 대상 `$APPDIR`로 rsync push | **git 무오염**, hostmover rsync 인프라 재사용, 배포 산출물 통제. dev 머신이 배포 주체일 때 최적 |
| B. `dist` 브랜치 | main엔 빌드본 없음, `dist`에만 web/build → `git clone -b dist` | SSH 한 줄. **단 커밋 누적으로 repo 증가**(orphan+force-push면 회피하나 번거로움). 배포 주체가 dev 머신이 아닐 때만 이점 |
| C. GitHub Release 아티팩트 | `web-build.tar.gz` 릴리스 첨부 → `curl … \| tar xz` | 버전 고정 깔끔, git 무오염. 릴리스 절차 필요 |

→ **A(rsync) 채택.** 사장님 dev 서버에 빌드본이 있고 hostmover가 rsync 도구라 dist 브랜치 이점이
   거의 없고 git만 커짐. **소스(app)는 git clone + web/build만 rsync** 또는 **pythonapp 통째 rsync** 둘 다 가능.
   대상에 rsync 없으면 기존 **tar 폴백** 사용.

- ✅ **web/build 전송 = rsync(§6.1 A)** 확정. git 무오염. (dist 브랜치는 git만 키워서 제외)

### 6.2 남은 질문

1. **코드 범위** — `pythonapp` 통째 rsync vs source는 git clone + `web/build`만 rsync. (둘 다 git 무오염)
2. **자동화 범위 / use_root** — root 풀 자동(v-*·systemd·nginx·SSL) vs 앱까지만 + 나머지 명령 출력(§3 폴백).
3. **포트 할당** — 스크립트 자동 탐색(8001~) vs 수동 입력.
4. **보안** — ADMIN_PASSWORD·DB비번이 스크립트 텍스트/원격 `ps`에 노출(기존 동작과 동일).
   자격증명 자체는 로컬 store.enc에 암호화 저장됨.

---

## 7. 상태

- [x] **Phase 1 구현 완료** — `EondInstall` 모델, `build_eondcms_resources/upload/finalize`(내장 nginx
  템플릿, SSL→tpl 순서), 설치 패널 + ①②③ 버튼 + 📋 미리보기 + 확인 모달. 테스트 20개(outer/inner bash -n).
  결정: ① pythonapp 통째 rsync, ② 포트 수동입력, use_root 필수.
- [ ] Phase 2 — 원클릭(①②③ 오케스트레이션), 포트 자동할당, 설치 후 자동 점검/복구 루프, private repo PAT
- [ ] 실서버 1회 설치 검증 (포트·systemd·nginx·SSL)
- 해결: nginx 템플릿(인라인), 설치 순서(SSL→tpl), 리소스 v-* 자동, use_root 폴백/대상 선택,
  **web/build 전송=rsync**
- **결정 대기**: §6.2 (1) 코드 범위(통째 rsync vs git+web/build), (2) 포트 자동/수동

## 8. DB 시드 적재 (필수 — 신규 사이트가 빈 DB면 부팅·관리자화면 실패)

eondcms 는 "기존 Rhymix(XE) DB 위에 얹히는" 구조 + 자체 `eond_` 스키마를 쓴다.
빈 DB 신규 사이트는 **두 SQL 을 순서대로 적재**해야 정상 동작한다. (둘 다 eondcms 레포 루트)

| # | 파일 | 내용 | 적재 |
|---|------|------|------|
| 1 | `rhymix_base.sql` | Rhymix 빈 베이스(xe_ 92테이블) + eondcms 확장컬럼(login_count 등) | `xe_modules` 이미 있으면 skip(데이터 보호) |
| 2 | `eond_schema.sql` | eondcms `eond_` 완성 스키마(92테이블, 구조만) | **DROP+CREATE — 빈 사이트만**(아래 가드) |

```bash
# 1) rhymix_base — xe_ 베이스
[ -f "$APPDIR/rhymix_base.sql" ] && [ "$(xe_modules 존재여부)" = 0 ] && mysql ... < rhymix_base.sql

# 2) eond_schema — DROP TABLE 포함이라 데이터 있으면 절대 적재 금지
ROWS=$(mysql ... -N -e "SELECT COUNT(*) FROM eond_projects" 2>/dev/null || echo 0)
if [ "$ROWS" = "0" ] && [ -f "$APPDIR/eond_schema.sql" ]; then
    mysql ... < eond_schema.sql        # 빈 사이트만
else
    echo "eond_ 데이터 있음 → schema 적재 스킵(데이터 보호)"
fi
```

**주의:**
- `eond_schema.sql` 은 `DROP TABLE IF EXISTS` 를 쓴다(이미 부팅된 사이트의 db_bootstrap 불완전 테이블을
  교체해야 하므로 `CREATE IF NOT EXISTS` 만으론 컬럼이 안 채워짐). → **데이터 있는 사이트엔 데이터 손실**.
- rhymix_base 의 "테이블 있으면 skip" 가드를 eond_schema 에 그대로 쓰면 안 됨 — 가드는 위처럼 **eond_ 데이터 유무**로.
- 스키마 변경 시 `eond_schema.sql` 재생성 필요 → eondcms `docs/rhymix-base-seed.md` 의 SHOW CREATE TABLE 스크립트.

## 9. 트러블슈팅 (실전 — 신규 설치)

| 증상 | 원인 | 해결 |
|------|------|------|
| uvicorn 포트 바인딩 실패 / 부팅 안 됨 | 빈 DB에 rhymix(xe_) 베이스 없음 | `rhymix_base.sql` 적재 |
| 로그인에서 `(1054) Unknown column 'xe_member.login_count'` | eondcms 확장컬럼 누락 | rhymix_base.sql(확장컬럼 포함) + db_bootstrap |
| 로그인 OK인데 **대시보드 빈 화면** | `eond_schema.sql` 미적재 → `eond_projects.deleted_at` 등 55테이블+71컬럼 누락 → stats 500 | `eond_schema.sql` 적재 + restart |
| eond_schema 적재했는데 그대로 | ① 파일이 clone 브랜치에 없음(`ls eond_schema.sql`), ② db_bootstrap이 불완전 테이블 선생성 → DROP+CREATE 필요 | 파일 포함 확인 / DROP 유지 |
| sudoers·exec·redirect | (워드프레스 복사 흐름. eondcms 설치와 무관) | 워드프레스 복사 문서 참고 |

> 진단 한 방(omg 서버 pythonapp 에서): `./.venv/bin/python` 으로 settings 의 DB 에 접속해
> `eond_` 테이블 수 + `eond_projects.deleted_at` 유무 확인 → 없으면 `eond_schema.sql` 적재.

## 부록 — 참고 자료

- DB 시드 상세/재생성: `~/dev/eondcms/docs/rhymix-base-seed.md` (rhymix_base.sql + eond_schema.sql)
- 수동 절차 전체: `~/dev/eondcms/docs/hestiacp-new-tenant-install.md`
- systemd/nginx 템플릿 원본: `~/dev/eondcms/.claude/hestiacp/{eondcms.service,eondcms.tpl,eondcms.stpl}`
- hostmover 패턴: `src/ops.rs`의 `OpKind`/`build()`/`Job`/`spawn()` (새 OpKind는 이 패턴 그대로)
- 데이터 모델: `src/model.rs`의 `Customer → Domain → {DomainAccess, Site asis, Site tobe, CmsAccess}`
