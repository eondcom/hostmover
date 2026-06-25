> **[통합 안내]** 이 문서의 내용은 **`eondcms-install-integration.md`(canonical)** 로 흡수·보강되었습니다.
> 최신 계획은 그 문서를 보세요. 아래는 원본 메모(히스토리 보존용).

---

# hostmover에 eondcms 설치 기능 추가 — 계획

> hostmover(SSH 기반 호스팅 마이그레이션 GUI)에 "원하는 계정/도메인에 eondcms를 설치"하는
> 작업을 추가한다. ASIS/TOBE 중 한쪽 Site 정보를 입력하면 그 서버/계정에 eondcms를 깐다.
> eondcms 설치 단계의 출처: `eondcms/docs/hestiacp-new-tenant-install.md`.

## 배경 / 왜 hostmover인가

- eondcms의 "호스트 생성 마법사"는 **eondcms 운영 서버가 떠 있어야** HestiaCP API로 사이트를 만든다.
  하지만 지금 목표는 *eondcms가 아직 없는 새 서버*에 까는 것 → 닭-달걀 문제.
- hostmover는 **SSH만 있으면 되는 독립 데스크탑 툴**이라 빈 서버 설치에 적합. (원래 요청도 hostmover)
- 방식은 eondcms 호스트 생성과 동일(도메인→DB→설치→SSL), 실행 주체만 hostmover.

## 작업 위치

- **이 hostmover repo에서 진행** (별도 git/cargo/CLAUDE 컨텍스트).
- eondcms 설치 지식이 필요하면 `eondcms/docs/hestiacp-new-tenant-install.md` 참고.

---

## 핵심 결정 #1 — web/build를 어떻게 서버에 넣나

`web/build`(SvelteKit 산출물, **7.3MB / 289파일**)는 eondcms에서 `.gitignore` 처리되어 있어
`git clone`만으론 프론트가 빠진다. 서버엔 Node를 안 깐다는 전제.

| 옵션 | 방식 | 평가 |
|------|------|------|
| **A. 전용 `dist` 브랜치 (추천)** | main엔 빌드본 없음, `dist`에만 web/build 포함. 설치 시 `git clone -b dist`. | git clone 한 방 + main 깨끗. **rsync 단계 불필요** |
| B. GitHub Release 아티팩트 | `web-build.tar.gz`를 릴리스 첨부 → 설치 시 `curl … | tar xz` | 버전 고정 깔끔, 릴리스 절차 필요 |
| C. main 직접 포함 | `.gitignore`에서 풀고 커밋 | 가장 단순하나 7.3MB×매빌드로 히스토리 오염 |

→ **A 채택 시** 설치 스크립트가 순수 SSH `git clone -b dist` 한 줄로 끝나 가장 단순.
   대가는 "빌드 후 dist 갱신"(server.sh에 1줄 추가)뿐.

**상태: 미확정 (A/B/C 중 택1, 추천 A)**

---

## 전체 설치 플로우 (단일 SSH 세션)

ASIS/TOBE 중 고른 Site의 `ip / ssh 로그인 / root / db_* / path` 정보로 실행.

1. (root, 선택) 도메인/DB 없으면 `v-add-web-domain` · `v-add-database` — MVP는 "이미 있음" 가정
2. `python3.11 -m venv .venv` (없을 때만)
3. 코드 가져오기 — `git clone -b dist` (결정#1=A) 또는 clone + web/build 전송
4. `poetry install --no-root --only main`
5. `.env` 생성:
   - `SECRET_KEY=$(openssl rand -hex 32)`
   - `FERNET_KEY=$(python -c "from cryptography.fernet import Fernet; print(Fernet.generate_key().decode())")`
   - `DATABASE_URL` = Site의 db_id/db_pw/db_name (HestiaCP면 `유저_` 접두어 주의)
   - `TABLE_PREFIX`, `ADMIN_USERNAME`, `ADMIN_PASSWORD`, `ENV=production`
   - `chmod 600 .env`
6. `alembic upgrade head`
7. (root) systemd `eondcms-<user>.service`(빈 포트 자동 탐색) + nginx 템플릿(포트 치환) + `v-add-letsencrypt-domain`
8. `curl 127.0.0.1:<port>` 확인

`use_root` 분기: root 계정 있으면 7단계 자동, 없으면 7단계는 **명령만 출력**(수동 실행 안내).

---

## 구현 항목

### 1. 데이터 모델 (`src/model.rs`)
- [ ] 설치 옵션 검토: `eondcms_port`(빈칸=자동), `hestia_user`, `install_domain` 또는 별도 `EondInstall` 구조체
- [ ] 기존 필드 최대 재사용 (ip·root·db_*·path)

### 2. 설치 작업 (`src/ops.rs`) — 핵심
- [ ] `OpKind::InstallEondcms` 추가
- [ ] `build_install_eondcms_job(site, domain, use_root)` — 위 플로우의 bash 스크립트 생성
      (기존 `sq()`/`ssh_e()`/`remote_cmd()`/`with_path()` 헬퍼 재사용)
- [ ] `use_root` 분기 (7단계 자동 vs 출력)
- [ ] bash 구문 테스트 — `generated_scripts_are_valid_bash`에 케이스 추가

### 3. UI (`src/app.rs`)
- [ ] 도메인 카드에 "eondcms 설치 ▸ ASIS / TOBE" 버튼 (설치 대상 Site 선택)
- [ ] 위험 작업 → 확인 모달 (기존 복원 모달 패턴 재사용)
- [ ] `spawn`으로 진행 로그 스트리밍 (기존 `LogMsg` 파이프 재사용)
- [ ] 완료 시 접속 URL/관리자 정보 안내

### 4. 검증
- [ ] `cargo build --release` + 단위 테스트
- [ ] 테스트 서버 실제 설치 1회 (포트·systemd·nginx·SSL 확인)

---

## 아직 정할 것 (구현 전 확정)

1. **web/build 전송** (결정#1): dist 브랜치(A, 추천) / Release(B) / main 포함(C)
2. **자동화 범위**: root 풀 자동(도메인·DB·systemd·nginx·SSL) / 앱까지만 + 나머지 안내
3. **도메인·DB 생성**: hostmover가 `v-add-*`로 생성 / 패널에서 미리 생성 가정(MVP)
4. **repo 접근**: `github.com/eondcom/eondcms`가 사설이면 PAT/deploy key 처리
5. **포트 할당**: 스크립트 자동 탐색(8001~) / 수동 입력

---

## 참고 자료

- eondcms 설치 가이드(수동 절차 전체): `eondcms/docs/hestiacp-new-tenant-install.md`
- eondcms systemd/nginx 템플릿 원본: `eondcms/.claude/hestiacp/eondcms.service`, `eondcms.tpl`, `eondcms.stpl`
- hostmover 작업 패턴: `src/ops.rs`의 `OpKind`/`build()`/`Job`/`spawn()` — 새 OpKind는 이 패턴을 그대로 따른다
- hostmover 데이터 모델: `src/model.rs`의 `Customer → Domain → {DomainAccess, Site asis, Site tobe, CmsAccess}`
