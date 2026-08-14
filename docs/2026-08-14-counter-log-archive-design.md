# `*_counter_log` 집계·아카이브 설계 및 파일럿 결과

> **상태**: `rokmc_haebyeongcom.hb_counter_log` 파일럿 완료(2026-08-14). 나머지 12개 테이블은 아직 미실행 — §4 참고.
> **원칙**: 상세 데이터를 그냥 지우지 않는다. "몇 년 몇 월에 몇 건"이라는 집계는 물론, **개별 방문 기록(IP·UA·시각)도 압축된 형태로 계속 보존**하면서 활성(hot) 테이블만 가볍게 만드는 것이 목표.

## 1. 배경

`hb_counter_log`(1,767만 행/2.5GB) 등 `*_counter_log` 테이블이 커진 것 자체가 오늘 인시던트의 배경 원인 중 하나였다(백업 시 `SELECT *`가 19초·2.7GB — [본 인시던트 문서](./2026-08-14-mars-db-lock-contention.md) §1.2 참고). InnoDB 전환으로 락 경합(구조적 원인)은 해결되지만, 테이블 자체가 계속 이 속도로 커지면:

- 백업(mysqldump) 시간·용량이 계속 늘어난다.
- `SELECT *`류 쿼리(관리자 통계 조회 등)의 절대 소요 시간은 여전히 테이블 크기에 비례해 늘어난다.
- 디스크 사용량도 무한정 증가한다.

→ 락 경합과는 별개로, **테이블 크기 자체를 관리하는 것**이 장기적으로 필요하다.

## 2. 목표 아키텍처

사이트별 3개 테이블 구조로 분리한다 (예: `haebyeong.com`, 원본 테이블명 `hb_counter_log`):

| 테이블 | 역할 | 엔진 | 보관 데이터 |
|---|---|---|---|
| `hb_counter_log` (기존, 유지) | **핫(hot) 테이블** — 최근 데이터만, 실서비스가 계속 읽고 씀 | InnoDB (전환 완료 후) | 최근 N개월 (안: 6개월) |
| `hb_counter_log_archive` (신규) | **콜드(cold) 아카이브** — 오래된 상세 행 그대로 이관, 읽기 전용 | ARCHIVE | N개월 이전 전체 (개별 행 보존) |
| `hb_counter_log_summary` (신규) | **월별 집계** — 아주 오래된 것도 "몇 건이었는지"는 항상 즉시 조회 가능 | InnoDB (작음) | 연-월 단위 카운트, 전체 기간 |

- `ARCHIVE` 엔진 특징: 매우 높은 압축률(이런 narrow-column 로그성 데이터는 보통 원본 대비 큰 폭으로 축소), `INSERT`/`SELECT`만 지원(`UPDATE`/`DELETE` 불가 — 애초에 안 건드릴 데이터라 적합), 인덱스는 PK만. "가끔 조회는 하지만 다시는 안 바꿀 로그"에 정확히 맞는 용도.
- 세 테이블을 합치면 **원본과 동일한 정보량**을 유지한다 — 상세 조회가 필요하면 archive를, 빠른 통계가 필요하면 summary를, 최근 데이터는 지금처럼 hot 테이블을 쓰면 된다.

## 3. 이관 절차 (실제 검증된 버전 — 파일럿에서 발견한 문제 2건 반영)

파일럿(§4) 도중 아래 절차의 최초 안(v1)에서 실제로 실패했던 두 가지를 먼저 기록한다:

1. **`year_month`는 예약어다.** MySQL/MariaDB의 `INTERVAL ... YEAR_MONTH` 구문에 쓰이는 예약 키워드라 컬럼명으로 못 쓴다(백틱으로 감싸거나 이름을 바꿔야 함). 아래 절차는 `ym`으로 변경.
2. **`ARCHIVE` 스토리지 엔진이 기본 설치돼 있지 않았다.** `SHOW ENGINES;`에 없었고, 플러그인 파일(`/usr/lib/mysql/plugin/ha_archive.so`)은 있지만 로드가 안 된 상태였음 — `INSTALL SONAME 'ha_archive';`로 최초 1회 활성화 필요(서버 전체에 한 번만 하면 됨, 이후 다른 테이블도 바로 사용 가능).

```sql
-- -1) (서버당 최초 1회만) ARCHIVE 엔진 활성화 — SHOW ENGINES로 이미 있으면 스킵
INSTALL SONAME 'ha_archive';

-- 0) 사전 백업은 InnoDB 전환 때와 동일 (mysqldump, 이미 있으면 재사용)

-- 1) 집계 테이블 생성 및 전체 기간 채우기 (원본 안 건드림, 읽기만)
CREATE TABLE hb_counter_log_summary (
  ym CHAR(6) PRIMARY KEY,   -- 'YYYYMM' (regdate가 VARCHAR(14) 'YYYYMMDDHHMMSS' 형식이라 LEFT()로 바로 추출)
  cnt INT UNSIGNED NOT NULL
) ENGINE=InnoDB;

INSERT INTO hb_counter_log_summary (ym, cnt)
SELECT LEFT(regdate,6), COUNT(*)
FROM hb_counter_log
GROUP BY LEFT(regdate,6);

-- 2) 아카이브 테이블 생성 (컬럼만 동일하게 명시 — ARCHIVE는 보조 인덱스 미지원이라 CREATE...LIKE로
--    인덱스까지 그대로 복사하면 실패하므로 인덱스 없이 새로 정의), 오래된 행 복사
CREATE TABLE hb_counter_log_archive (
  site_srl bigint(11) NOT NULL,
  ipaddress varchar(250) NOT NULL,
  regdate varchar(14) DEFAULT NULL,
  user_agent varchar(250) DEFAULT NULL
) ENGINE=ARCHIVE;

-- 컷오프는 한 번 계산해서 변수/파일에 저장해두고 이후 단계(삭제)에서도 반드시 같은 값을 재사용할 것
-- (재계산하면 그 사이 시간차만큼 값이 달라져 archive와 delete 대상이 어긋날 수 있음)
SET @cutoff = DATE_FORMAT(DATE_SUB(NOW(), INTERVAL 3 MONTH), '%Y%m%d%H%i%s');

INSERT INTO hb_counter_log_archive
SELECT * FROM hb_counter_log
WHERE regdate < @cutoff;

-- 3) 검증 — 반드시 맞아야 다음 단계로 진행
--    (archive 행수 + hot에 남을 최근 행수) == 원본 전체 행수
SELECT
  (SELECT COUNT(*) FROM hb_counter_log) AS 원본_전체,
  (SELECT COUNT(*) FROM hb_counter_log_archive) AS 아카이브,
  (SELECT COUNT(*) FROM hb_counter_log WHERE regdate >= @cutoff) AS 남을것,
  (SELECT COUNT(*) FROM hb_counter_log_archive) + (SELECT COUNT(*) FROM hb_counter_log WHERE regdate >= @cutoff) AS 합계검산;

-- 4) 검증 통과한 경우에만 — 유일한 실제 삭제 단계 (hot 테이블에서만 삭제, archive엔 이미 보존됨)
--    @cutoff는 2)에서 쓴 것과 반드시 동일한 값(세션 유지 또는 저장해둔 값 재사용)
DELETE FROM hb_counter_log WHERE regdate < @cutoff;
OPTIMIZE TABLE hb_counter_log;   -- 삭제로 생긴 여유 공간을 파일에서도 회수
```

- **4번(DELETE)이 유일하게 되돌릴 수 없는 단계**다 — 단, 그 시점엔 이미 archive 테이블에 동일 데이터가 안전하게 있고 2)~3)에서 행수까지 대조 확인한 뒤이므로 실질적 데이터 손실은 없다. 그래도 이 단계 직전에 한 번 더 사람이 확인하고 진행해야 한다.

## 4. 파일럿 실행 결과 — `rokmc_haebyeongcom.hb_counter_log` (2026-08-14 완료)

**보관 기간 결정**: 최초 안(6개월)으로 집계해보니 **2026년 6월에 트래픽이 유독 튀어(690만 건, 다른 달의 5~10배)** 6개월 기준으로도 핫 테이블에 1,165만 행(원본의 66%)이 남아 용량 절감 효과가 작았음. **3개월로 조정**해서 재실행.

| 단계 | 결과 |
|---|---|
| 월별 집계(`hb_counter_log_summary`) | 정상 생성, 전체 기간(2021-01 ~ 2026-08) 커버 |
| 아카이브 이관 (3개월 이전 대상) | 7,073,479행 → `hb_counter_log_archive` |
| 검증 | 원본 전체(17,681,101) = 아카이브(7,073,479) + 남을 것(10,607,622) — **정확히 일치** |
| 삭제 직전 재확인 | 삭제 대상 행수(7,073,479) == 이미 아카이브된 행수(7,073,479) — **정확히 일치** |
| 삭제 실행 | 18초 |
| 삭제 후 검증 | 남은 행 10,607,636 + 아카이브 7,073,479 ≈ 집계 합계(17,681,100) — 오차 15행은 삭제 진행 중 실시간 방문 트래픽으로 자연 증가(데이터 손실 아님) |
| OPTIMIZE TABLE | 28초 (InnoDB는 내부적으로 "recreate + analyze"로 처리됨 — 정상 메시지) |
| **용량 변화** | `hb_counter_log`: 3,290MB → **1,944MB** (41% 감소) / `hb_counter_log_archive`: **113MB**(707만 행이 gzip 압축) / `hb_counter_log_summary`: 1MB 미만 |
| 사이트 정상 동작 | `haebyeong.com` 301 리다이렉트 정상 응답(0.88초) 재확인 |

**핵심 확인 사항**: ARCHIVE 엔진의 압축률이 기대 이상 — InnoDB로 707만 행을 유지했다면 1GB를 훌쩍 넘었을 것을 113MB로 압축. 전체 디스크 사용량도 3,290MB → 2,057MB(핫+아카이브 합)로 실질 절감됨.

## 5. 나머지 12개 테이블 일괄 적용 결과 (2026-08-14 완료)

파일럿에서 검증된 절차(§3)로 12개 테이블 전부 동일 기준(3개월 보관)으로 일괄 실행. 실행 전 월별 집계를 훑어 `pooyas_pooyas.xe_counter_log`(2026-07 953만 건, 2026-08 513만 건)와 `rokmc_rokmcgolf.mg_counter_log`(2026-07 232만 건)에 최근 트래픽 폭증이 있음을 미리 확인함 — 이 둘은 3개월 기준으로도 감소폭이 작을 것으로 예상하고 진행.

**실행 중 버그 1건 추가 발견**: `yncare_xe.xe_counter_log`·`eond_yncare.xe_counter_log` 두 테이블만 컬럼 순서가 달랐다(`ipaddress, regdate, user_agent, site_srl` — 다른 테이블은 `site_srl`이 첫 컬럼). `INSERT ... SELECT *`는 위치 기준 매칭이라 `ipaddress`(문자열)가 아카이브 테이블의 `site_srl`(정수) 자리에 들어가려다 `ERROR 1265 Data truncated`로 실패. **`--force` 없이 실행해서 에러 즉시 스크립트가 중단됐고, 삭제 단계 전이라 원본은 전혀 안 건드려짐** — 컬럼명을 명시(`INSERT INTO archive (site_srl, ipaddress, regdate, user_agent) SELECT site_srl, ipaddress, regdate, user_agent FROM ...`)해서 재실행, 성공.

| 테이블 | 원본 행수 | 남은 행수(핫) | 아카이브 행수 | 감소율 | 비고 |
|---|---:|---:|---:|---:|---|
| pooyas_pooyas.xe_counter_log | 17,447,524 | 15,409,705 | 2,037,913 | 12% | 최근 폭증(7·8월)으로 감소폭 작음 |
| rokmc_namwon.nw_counter_log | 11,523,193 | 376,054 | 11,147,139 | 96.7% | |
| rokmc_haebyeongcokr.hb_counter_log | 6,354,559 | 519,732 | 5,834,828 | 91.8% | |
| rokmc_rokmcgolf.mg_counter_log | 4,255,188 | 3,399,218 | 855,970 | 20% | 최근 폭증(7월)으로 감소폭 작음 |
| rokmc_bsuperman.sp_counter_log | 2,392,069 | 454,488 | 1,937,581 | 81% | |
| insoo_bbib.xe_counter_log | 2,215,220 | 735,429 | 1,479,792 | 66.8% | |
| yncare_xe.xe_counter_log | 2,039,139 | 7,558 | 2,031,581 | 99.6% | 컬럼순서 버그 수정 후 성공 |
| eond_yncare.xe_counter_log | 2,023,301 | 0 | 2,023,301 | 100% | 최근 3개월 활동 없음(사실상 비활성) |
| fgsmc_fgsmc.fgsmc_counter_log | 1,517,993 | 473,759 | 1,044,234 | 68.8% | |
| rokmc_jjhyanggyo.xe_counter_log | 1,302,861 | 676,168 | 626,693 | 48.1% | |
| swslr_swslr.xe_counter_log | 1,089,747 | 630,169 | 459,578 | 42.2% | |
| rokmc_rokmcva.rv_counter_log | 939,893 | 9,848 | 930,045 | 99% | |

**합계**: 12개 테이블에서 약 3,040만 행이 아카이브로 이관(원본에서 삭제, 데이터는 압축 보존). §4의 `hb_counter_log` 707만 행까지 합치면 오늘 하루 총 **약 3,748만 행**을 hot 테이블에서 archive로 옮김.

**전부 검증 통과**(원본 = 아카이브 + 남을 것, 삭제 후 재확인까지 일치), 에러 없이 완료. `pooyas.com`·`namwon.net`·`yncare.net`·`swslr.com` 등 재확인 결과 전부 정상 응답(0.1~0.4초).

## 6. 서버 전체 재조사 후 나머지 13개 테이블 (2026-08-14 완료)

§5까지 처리한 13개(§4 파일럿 1개 + §5 배치 12개)는 애초에 "100MB 이상"으로 뽑았던 우선순위 리스트 기준이었다. 서버 전체를 `%counter_log` 패턴으로 재조사한 결과, 그 기준에 못 미쳐 빠졌던(또는 그사이 커진) 테이블이 더 있었음이 드러났다 — 예: `neosol_sclink`·`neosol_neosol`은 최초 조사 때 1~8MB였으나 재조사 시점엔 100MB 이상으로 성장.

`eond_eond.xe_counter_log`(1,035MB, 운영자 자체 계정)는 **사용자 지시로 이번엔 제외**. 그 외 1만 행 이상인 13개 테이블을 동일 절차(3개월 보관)로 처리(0~2행짜리 빈 테이블 다수는 처리 실익 없어 스킵):

**사전 스키마 점검에서 버그 재발 방지**: 13개 중 `rokmc_hgstudio.rx_counter_log`만 컬럼 구성이 달랐다(`id, site_srl, regdate, ipaddress, user_agent` — PK `id` 컬럼 추가 + 순서도 다름). §5에서 겪은 컬럼 순서 버그를 반복하지 않도록, 이번엔 전 테이블에 대해 사전에 컬럼 순서를 조회하고 **모든 INSERT에 컬럼명을 명시**(`INSERT INTO archive (site_srl, ipaddress, regdate, user_agent) SELECT site_srl, ipaddress, regdate, user_agent FROM ...`)해서 실행 — 13개 전부 에러 없이 성공.

| 테이블 | 원본 행수 | 남은 행수 | 아카이브 행수 | 비고 |
|---|---:|---:|---:|---|
| oracall_jslocal.rx_counter_log | 2,135,693 | 214,634 | 1,921,060 | |
| neosol_sclink.xe_counter_log | 577,223 | 89,293 | 487,930 | |
| neosol_neosol.xe_counter_log | 568,508 | 393,122 | 175,386 | |
| daoom_daoom.xe_counter_log | 388,751 | 117,162 | 271,589 | |
| itmang_itmang.xe_counter_log | 222,571 | 0 | 222,571 | 최근 3개월 활동 없음 |
| hani_hani.xe_counter_log | 218,032 | 47,205 | 170,827 | allofhani.com |
| rokmc_ibsq.iq_counter_log | 197,739 | 28,804 | 168,935 | |
| rokmc_hanjischool.hs_counter_log | 153,231 | 20,402 | 132,829 | |
| oracall_dev.rx_counter_log | 79,490 | 0 | 79,490 | 최근 3개월 활동 없음 |
| rokmc_jbmice.rx_counter_log | 26,858 | 5,214 | 21,644 | |
| rokmc_hgstudio.rx_counter_log | 16,720 | 14,363 | 2,357 | 스키마 다름(§6 참고), 정상 처리됨 |
| eond_aithres.rx_counter_log | 12,443 | 0 | 12,443 | 최근 3개월 활동 없음 |
| rokmc_dmovie.rx_counter_log | 12,118 | 2,249 | 9,869 | |

전부 검증 통과(원본 = 아카이브 + 남을 것). `jslocal.org`·`itmang.co.kr`·`allofhani.com` 재확인 정상(200 OK).

**서버 전체 counter_log 현황(2026-08-14 기준)**: 유의미한 크기의 테이블은 `eond_eond.xe_counter_log`(1,035MB, 미처리·제외됨) 1개만 남았다. 나머지는 전부 처리했거나(총 §4+§5+§6 = 26개 테이블, 약 3,780만 행 아카이브) 원래 빈 테이블(0~2행)이라 처리 불필요.

## 7. 진행 순서 — 전체 완료

1. ~~InnoDB 전환(락 경합 해결)~~ — 완료.
2. ~~파일럿 1개(`hb_counter_log`)로 절차 검증~~ — 완료, 버그 2건 수정(§3).
3. ~~우선순위 12개 테이블 일괄 적용~~ — 완료, 추가 버그 1건 수정·문서화(§5).
4. ~~서버 전체 재조사 후 나머지 13개(eond_eond 제외)~~ — 완료, 스키마 사전 점검으로 버그 재발 방지(§6).
5. 며칠 관찰(사이트 정상 동작, 관리자 통계 화면 이상 없는지) — 남은 할 일. 특히 `eond_yncare`·`itmang_itmang`·`oracall_dev`·`eond_aithres`처럼 핫 테이블이 0행이 된 곳은 관리자 화면에서 "최근 방문자 없음"으로 보이는 게 맞는지 확인 필요.
6. `eond_eond.xe_counter_log`(1,035MB) — 사용자 지시로 제외한 상태. 필요 시 별도 진행.
7. §5의 열린 질문(archive 조회 경로, 고객 고지 여부)은 여전히 미결 — 필요 시 별도 진행.
