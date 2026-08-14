# `*_counter_log` 집계·아카이브 설계 (안 — 미실행)

> **상태**: 설계만. 아직 실행 안 함. [`2026-08-14-mars-db-lock-contention.md`](./2026-08-14-mars-db-lock-contention.md)의 InnoDB 전환(락 경합 근본 해결)과는 별개 작업이며, 그쪽이 끝난 뒤 여유를 갖고 진행한다.
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

## 3. 이관 절차 (사이트 1개 기준, 안)

```sql
-- 0) 사전 백업은 InnoDB 전환 때와 동일 (mysqldump, 이미 있으면 재사용)

-- 1) 집계 테이블 생성 및 전체 기간 채우기 (원본 안 건드림, 읽기만)
CREATE TABLE hb_counter_log_summary (
  year_month CHAR(7) PRIMARY KEY,   -- 'YYYY-MM'
  cnt INT UNSIGNED NOT NULL
) ENGINE=InnoDB;

INSERT INTO hb_counter_log_summary (year_month, cnt)
SELECT DATE_FORMAT(regdate, '%Y-%m'), COUNT(*)
FROM hb_counter_log
GROUP BY DATE_FORMAT(regdate, '%Y-%m');

-- 2) 아카이브 테이블 생성 (원본과 동일 스키마), 오래된 행 복사 (원본 안 지움 — 아직은 복사만)
CREATE TABLE hb_counter_log_archive LIKE hb_counter_log;
ALTER TABLE hb_counter_log_archive ENGINE=ARCHIVE;

INSERT INTO hb_counter_log_archive
SELECT * FROM hb_counter_log
WHERE regdate < DATE_SUB(NOW(), INTERVAL 6 MONTH);

-- 3) 검증 — 반드시 셋 다 맞아야 다음 단계로 진행
--    (archive 행수 + hot에 남을 최근 행수) == 원본 전체 행수
SELECT COUNT(*) FROM hb_counter_log_archive;                                    -- A
SELECT COUNT(*) FROM hb_counter_log WHERE regdate >= DATE_SUB(NOW(), INTERVAL 6 MONTH); -- B
SELECT COUNT(*) FROM hb_counter_log;                                            -- A + B 와 같아야 함(이관 전 원본 전체)

-- 4) 검증 통과한 경우에만 — 유일한 실제 삭제 단계 (hot 테이블에서만 삭제, archive엔 이미 보존됨)
DELETE FROM hb_counter_log WHERE regdate < DATE_SUB(NOW(), INTERVAL 6 MONTH);
OPTIMIZE TABLE hb_counter_log;   -- 삭제로 생긴 여유 공간을 파일에서도 회수
```

- **4번(DELETE)이 유일하게 되돌릴 수 없는 단계**다 — 단, 그 시점엔 이미 archive 테이블에 동일 데이터가 안전하게 있고 2)~3)에서 행수까지 대조 확인한 뒤이므로 실질적 데이터 손실은 없다. 그래도 이 단계 직전에 한 번 더 사람이 확인하고 진행해야 한다.

## 4. 열린 질문 (실행 전 결정 필요)

1. **보관 기간(hot 테이블에 남길 기간)**: 안은 6개월. 사이트 관리자가 "최근 몇 개월치"를 실제로 조회/통계에 쓰는지 확인 후 조정.
2. **archive 데이터의 조회 경로**: XE/Rhymix 관리자 화면이 `hb_counter_log` 하나만 보고 있다면, 오래된 데이터를 보려면 별도 조회(직접 SQL 또는 관리자 화면에 `UNION` 뷰 추가)가 필요할 수 있음 — 고객이 오래된 방문자 통계를 실제로 UI에서 봐야 하는지 확인 필요.
3. **고객 고지 여부**: 상세 데이터 자체는 보존되지만 "최근 화면에서 안 보이게" 되는 변화이므로, 사전 고지가 필요한지 판단.
4. **대상 범위**: [인시던트 문서](./2026-08-14-mars-db-lock-contention.md) §2.3의 13개 테이블 전부 할지, 가장 큰 것(haebyeong.com, pooyas.com 등) 먼저 파일럿으로 할지.

## 5. 진행 순서 (제안)

1. InnoDB 전환(락 경합 해결) 먼저 전부 완료 — 진행 중.
2. 위 열린 질문 답 정하기(특히 보관 기간, archive 조회 필요 여부).
3. 가장 큰 테이블 1개(`hb_counter_log`)로 파일럿 — 이관 후 며칠 관찰(사이트 정상 동작, 관리자 통계 화면 이상 없는지).
4. 문제 없으면 나머지 12개 테이블에 순차 적용.
