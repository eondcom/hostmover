# mars.eond.com 사이트 불안정 인시던트 — 봇/트래픽 폭주 + MyISAM 카운터로그 락 경합

- **서버**: mars.eond.com (`121.124.124.13`)
- **최초 신고**: 2026-08-14, 고객이 `neosol.co.kr`·`sclink.co.kr` 접속 불안정 호소
- **조사/조치**: 2026-08-14
- **결론 요약**: 원인이 두 층으로 겹쳐 있었다.
  1. **봇/트래픽 폭주로 인한 웹 스택(Apache/nginx) 워커 고갈** — 즉시 조치 완료.
  2. **`*_counter_log`(방문자 로그) 테이블이 정리된 적 없이 최대 1,760만 행·2.5GB까지 비대해졌고, 대부분 MyISAM(테이블 단위 락) 엔진** — 하루 종일 짧은 테이블 락 대기가 누적되다가 결국 커넥션이 밀려 "느려짐"으로 체감. DB 재시작은 쌓인 락/커넥션을 강제로 비워 일시적으로만 해소함 — **재발성 문제, 아직 미해결.** 이 문서 하단에 InnoDB 전환 계획을 남긴다.

---

## 1. 증상과 진단 과정

### 1.1 웹 스택 레이어 (해결됨)

- `neosol.co.kr`/`sclink.co.kr`: nginx 로그에 `upstream timed out ... while SSL handshaking to upstream`가 반복(당일 sclink 594건, neosol 108건). TCP/TLS는 붙는데 Apache 백엔드(내부 8443)가 응답을 안 줌.
- 원인: GPTBot·MJ12bot·Baiduspider·Bing 대역 등 크롤러가 무거운 게시판 목록 페이지를 초 단위로 반복 요청 → Apache `MaxRequestWorkers=150`이 상시 포화(154개 프로세스 관측, 한도 초과 근접).
- `allofhani.com`은 별개로 `45.38.0.0/16`·`45.39.0.0/16`(미국 Subnet Digital LLC, 프록시/봇넷 대여업체로 흔히 남용되는 대역)에서 `index.php?mid=notice` 한 페이지에 분산 플러딩 — 최근 500건 에러로그 중 498건이 이 두 대역.

**조치 (완료):**

| 조치 | 대상 | 방법 |
|---|---|---|
| 봇 UA 차단 (`$bad_bot` map, `if ($bad_bot) return 403;`) | 83개 도메인 (SSL vhost에 `nginx.ssl.conf_*` 커스텀 include 훅이 있는 것만) | `/home/<user>/conf/web/<domain>/nginx.ssl.conf_blockbots` 생성, `nginx -t` → `reload` |
| Apache `MaxRequestWorkers` 150 → 400 (+ `ServerLimit 400` 명시) | 서버 전체 (`/etc/apache2/mods-available/mpm_prefork.conf`) | 백업(`mpm_prefork.conf.bak-20260814`) 후 수정, `apache2ctl configtest` → `systemctl reload apache2` |
| `45.38.0.0/16`, `45.39.0.0/16` 대역 차단 | 서버 전체 (`/etc/nginx/conf.d/block-scrapers.conf`, 2026-07-07 OVH/Hetzner 차단과 동일 패턴) | 백업(`block-scrapers.conf.bak-20260814`) 후 `deny` 라인 추가, `nginx -t` → `reload` |

- 봇 차단 훅이 없는 9개 구형 템플릿 도메인(`aiui.kr`, `card.downloadetc1.com`, `church.cimin.kr`, `cimin.kr`, `dev.unopenegg.com`, `englink.co.kr`, `innostv.co.kr`, `itmang.co.kr`, `lllayer.cimin.kr`)은 이번엔 건드리지 않음 — 필요시 별도 진행.
- 조치 후 neosol/sclink/allofhani 등 재확인 결과 200 OK로 복구 확인.

### 1.2 DB 레이어 (미해결 — 이 문서의 본론)

웹 스택 조치 이후에도 사용자가 "DB 재시작하니 빨라짐"이라고 보고. 재시작 직후(가동 9분 시점) 상태를 확인한 결과:

```
Table_locks_waited: 2065      (재시작 9분 만에, 초당 약 3.75건 페이스)
Table_locks_immediate: 266219
Connections: 14412            (9분간, 초당 약 26개 — 매 요청마다 신규 접속)
Threads_connected: 44 / max_connections 500
```

슬로우쿼리 로그(`long_query_time=2.0`)에서 재시작 전 마지막 기록(당일 05:39~06:00, 백업 창으로 추정)을 확인:

```sql
-- User@Host: root[root] @ localhost   (사이트 PHP 코드가 아니라 root/로컬에서 실행됨 — mysqldump로 추정)
SELECT /*!40001 SQL_NO_CACHE */ `site_srl`,`ipaddress`,`regdate`,`user_agent` FROM `hb_counter_log`;
-- Query_time: 19.47s  Rows_sent=Rows_examined: 17,667,607  Bytes_sent: 2.69GB  Full_scan: Yes
```

같은 패턴(`SELECT * FROM *_counter_log`, WHERE/LIMIT 없음, `Full_scan: Yes`)이 여러 고객 DB에서 반복 확인됨. hostmover 소스(`src/`)에는 이런 쿼리가 없음을 확인 — hostmover가 원인은 아니고, 백업(mysqldump) 과정에서 각 테이블을 통째로 읽는 정상 동작이 비대해진 MyISAM 테이블을 만나 문제가 커진 것으로 판단.

**테이블 실태 (용량순 상위, `information_schema.tables` 기준):**

| DB | 테이블 | 행 수 | 용량 | 엔진 |
|---|---|---|---|---|
| rokmc_haebyeongcom | hb_counter_log | 17,675,836 | 2,577 MB | MyISAM |
| pooyas_pooyas | xe_counter_log | 17,318,104 | 2,373 MB | MyISAM |
| rokmc_namwon | nw_counter_log | 11,523,023 | 1,674 MB | MyISAM |
| eond_eond | xe_counter_log | 5,724,961 | 1,035 MB | InnoDB |
| rokmc_haebyeongcokr | hb_counter_log | 6,353,257 | 909 MB | MyISAM |
| rokmc_rokmcgolf | mg_counter_log | 4,248,711 | 591 MB | MyISAM |
| rokmc_bsuperman | sp_counter_log | 2,388,775 | 341 MB | MyISAM |
| oracall_jslocal | rx_counter_log | 1,879,319 | 337 MB | InnoDB |
| insoo_bbib | xe_counter_log | 2,211,884 | 306 MB | MyISAM |
| yncare_xe | xe_counter_log | 2,039,111 | 290 MB | MyISAM |
| eond_yncare | xe_counter_log | 2,023,301 | 288 MB | MyISAM |
| fgsmc_fgsmc | fgsmc_counter_log | 1,515,989 | 219 MB | MyISAM |
| rokmc_jjhyanggyo | xe_counter_log | 1,298,787 | 191 MB | MyISAM |
| swslr_swslr | xe_counter_log | 1,077,098 | 153 MB | MyISAM |
| rokmc_rokmcva | rv_counter_log | 939,855 | 138 MB | MyISAM |

전 DB 통틀어 테이블 엔진 분포: InnoDB 5,437 / MyISAM 3,236 / MEMORY 4 — **약 37%가 여전히 MyISAM.**

### 1.3 인과 관계 정리

1. XE/Rhymix의 `*_counter_log`는 방문 기록을 계속 INSERT만 하고 정리(pruning)가 없어 무한정 커짐.
2. 대부분 MyISAM 엔진 → **테이블 전체 단위 락**. 읽기(백업의 `SELECT *`, 관리자 통계 조회 등)가 길게 걸리는 동안 같은 테이블의 동시 INSERT(실시간 방문자 기록)가 막힘.
3. 새벽 백업 창엔 초 단위 락(최대 19초)으로 크게 터지고, 평시엔 짧은(2초 미만이라 슬로우로그에 안 잡히는) 락 대기가 초당 여러 건씩 하루 종일 누적.
4. 누적된 락 대기가 스레드/커넥션 적체로 이어지고, 결국 체감상 "서버가 느려짐" → **DB 재시작은 쌓인 락·커넥션을 한 번에 비워 일시적으로만 해소**. 테이블 구조가 그대로라 시간이 지나면 다시 쌓임 — **재발 예상.**

---

## 2. 해결 계획 — MyISAM → InnoDB 전환 (데이터 삭제 없이)

> **사용자 결정 사항**: 데이터는 지우지 않는다(로그 삭제 대신 엔진 전환). 전환 전 전체 DB 백업 선행. 사이트/테이블 단위로 하나씩 순차 진행 — 한 번에 일괄 처리하지 않는다.

### 2.1 원칙

- **삭제 없음**: `DELETE`/`TRUNCATE` 없이 `ALTER TABLE ... ENGINE=InnoDB`만 수행. 행 수·데이터는 그대로 유지되고 락 방식만 테이블 단위 → 행 단위로 바뀐다.
- **사전 전체 백업 필수**: 전환 자체는 되돌릴 수 있는 작업(`ENGINE=MyISAM`으로 재변환 가능)이지만, 대용량 테이블의 `ALTER TABLE`은 시간이 걸리고 그 사이 디스크 I/O·공간을 크게 쓰므로, 만약을 대비해 전환 대상 DB는 개별적으로 백업해 둔다.
- **순차 진행**: 한 번에 여러 테이블을 동시에 돌리지 않는다. 하나 끝나고 사이트 정상 동작(로그인/글쓰기/방문자 카운트) 확인 후 다음으로.
- **트래픽 적은 시간대**: 대용량 테이블(2GB급)은 전환 중 몇 분~수십 분 해당 테이블이 잠기므로 새벽 시간대 진행 권장.

### 2.2 절차 (테이블 1개 기준)

```bash
# 0) 사전 확인: 디스크 여유공간 — InnoDB 변환은 새 테이블을 만들고 나서 기존 것을 지우므로
#    순간적으로 "원본 + 신규" 두 배 용량이 필요할 수 있음. /home 파티션 여유(140G, 69% 사용) 확인.
df -h /home

# 1) 대상 DB 개별 백업 (mysqldump, gzip)
mysqldump --single-transaction=false --lock-tables=true <db_name> > /backup/manual/<db_name>-preinnodb-$(date +%Y%m%d).sql
gzip /backup/manual/<db_name>-preinnodb-$(date +%Y%m%d).sql

# 2) 전환 전 상태 기록 (되돌릴 때 대조용)
mysql -e "SELECT COUNT(*) FROM <db_name>.<table_name>;"
mysql -e "CHECKSUM TABLE <db_name>.<table_name>;"

# 3) 전환 실행 (테이블 1개, 트래픽 적은 시간대)
mysql -e "ALTER TABLE <db_name>.<table_name> ENGINE=InnoDB;"

# 4) 검증
mysql -e "SHOW TABLE STATUS FROM <db_name> LIKE '<table_name>';"   # Engine=InnoDB 확인
mysql -e "SELECT COUNT(*) FROM <db_name>.<table_name>;"            # 행 수 전환 전과 동일한지
mysql -e "CHECKSUM TABLE <db_name>.<table_name>;"                  # (엔진이 바뀌면 체크섬 알고리즘도 달라질 수 있어 참고용)

# 5) 해당 사이트 실제 동작 확인 (hostmover 접속 테스트 또는 curl로 페이지 응답)
curl -sS -o /dev/null -w "status:%{http_code}\n" https://<domain>/
```

- **되돌리기**: 문제가 있으면 `ALTER TABLE ... ENGINE=MyISAM;`으로 즉시 원복 가능(데이터 보존됨). 그래도 이상하면 1)에서 만든 백업으로 복원.

### 2.3 순서 (용량 큰 것부터, 영향도 큰 순서)

DB 재시작 직후에도 락 경합이 바로 재발한 것으로 보아, **가장 큰 테이블부터** 처리하는 게 체감 효과가 가장 빠르다.

1. `rokmc_haebyeongcom.hb_counter_log` (2,577MB)
2. `pooyas_pooyas.xe_counter_log` (2,373MB)
3. `rokmc_namwon.nw_counter_log` (1,674MB)
4. `rokmc_haebyeongcokr.hb_counter_log` (909MB)
5. `rokmc_rokmcgolf.mg_counter_log` (591MB)
6. `rokmc_bsuperman.sp_counter_log` (341MB)
7. `insoo_bbib.xe_counter_log` (306MB)
8. `yncare_xe.xe_counter_log` (290MB)
9. `eond_yncare.xe_counter_log` (288MB)
10. `fgsmc_fgsmc.fgsmc_counter_log` (219MB)
11. `rokmc_jjhyanggyo.xe_counter_log` (191MB)
12. `swslr_swslr.xe_counter_log` (153MB)
13. `rokmc_rokmcva.rv_counter_log` (138MB)
14. (그 외 나머지 MyISAM 테이블 — 전체 재조사 후 100MB 이상만 우선, 이하는 락 경합 영향 적어 후순위)

이 중 1~3번(`haebyeong.com`, `pooyas.com`, `namwon.net`)은 오늘 이미 웹 스택 문제로 함께 등장했던 계정들이라 우선순위가 높다.

### 2.4 진행 후 검증 지표

전환 작업 완료 후 다음으로 개선 여부 확인:

```sql
SHOW GLOBAL STATUS LIKE 'Table_locks_waited';   -- 시간당 증가폭이 눈에 띄게 줄어야 함
SHOW GLOBAL STATUS LIKE 'Table_locks_immediate';
```

- 전환 전: 재시작 9분 만에 `Table_locks_waited` 2,065건(초당 ~3.75건).
- 목표: 전환 완료 구간(1~13번)에 한해 이 수치의 증가 속도가 뚜렷이 감소하는지 관찰.

### 2.5 실행 결과 (2026-08-14 완료)

**counter_log 13개 테이블** (§2.3 목록 전체) — 전부 백업 후 `ALTER TABLE ENGINE=InnoDB` 성공. 행수는 13개 중 7개 완전 일치, 6개는 전환 도중 실사용자 트래픽으로 +1~+6행 증가(감소는 0건 — 데이터 손실 없음, `information_schema.tables.TABLE_ROWS`는 InnoDB에서 추정치라 참고용일 뿐 검증엔 `SELECT COUNT(*)` 사용). 가장 큰 `hb_counter_log`(2.5GB, 1,767만 행)도 94초 만에 무중단 전환.

**`*_documents` 계열 29개 테이블 추가 전환** — counter_log 전환만으로는 `Table_locks_waited` 증가율이 초당 3.75건→2.3건(39% 감소)에 그쳐, 게시글 본문 테이블(`xe_documents`류)도 같은 방식으로 전환. 가장 큰 `hani_hani.xe_documents`(allofhani.com, 2.5GB)는 27초, 나머지는 전부 수 초 이내. 29개 전부 행수 완전 일치, `ENGINE=InnoDB` 확인.

**최종 효과**: `Table_locks_waited` 증가율 **초당 3.75건 → 초당 0.03건 (98%+ 감소)**. `allofhani.com`·`haebyeong.com`·`namwon.net`·`neosol.co.kr`·`sclink.co.kr`·`pooyas.com` 전부 0.1~1.1초 내 정상 응답 재확인.

전체 백업 파일: `/backup/manual/<db>-preinnodb-20260814.sql.gz` (DB 단위, counter_log/documents 전환 대상 DB 전부 커버, 백업 있으면 재사용).

### 2.6 서버 전체 잔여 MyISAM 일괄 전환 (2026-08-14 완료)

우선순위 테이블(counter_log·documents, 42개) 전환 후에도 서버엔 여전히 MyISAM 테이블 3,193개(총 2.7GB, 33개 DB에 분산 — 대부분 XE/Rhymix 모듈별 소규모 설정·메타 테이블)가 남아 있었다. FULLTEXT 인덱스는 전무함을 확인 후, DB 단위로 묶어 일괄 전환:

- 33개 DB, DB별로 (기존 백업 재사용 또는 신규 백업) → 해당 DB의 모든 MyISAM 테이블을 **하나의 mysql 세션에서 일괄 `ALTER TABLE ... ENGINE=InnoDB`** 실행(테이블마다 개별 접속하지 않아 SSH/커넥션 오버헤드 최소화) → 전환 후 남은 MyISAM 개수 검증.
- **33개 DB 전부 `[OK]`, 에러/경고 0건.**
- 서버 전체 최종 확인: `SELECT ENGINE, COUNT(*) FROM information_schema.tables ...` → **MyISAM 0개**, InnoDB 8,673개, MEMORY 4개(세션/캐시용, 정상).
- `Table_locks_waited` 최종 측정: 30초 구간 **증가 0건** (이전 §2.5 단계에서 초당 0.03건 → 사실상 완전 해소).
- 여러 도메인(`swslr.com`, `ibsq.co.kr`, `eond.com` 등) curl 재확인 — 전부 0.1~0.2초 내 정상 응답.
- 점검 중 `makkuk.eond.com`이 403을 반환하는 걸 발견했으나, 로그 확인 결과 **2026-08-09부터(오늘 작업 훨씬 이전) 그 도메인 자체 nginx 설정의 `deny all;`로 인한 기존 상태**이며 오늘 작업과 무관함을 확인함(그 사이트 자체가 PHP Fatal Error로 이미 깨져 있어 운영자가 막아둔 것으로 추정).

### 2.7 향후 재발 방지 (별도 논의 필요, 이번 범위 밖)

- XE/Rhymix 신규 설치 시 `*_counter_log`류를 기본 InnoDB로 생성하도록 설정/스킨 점검.
- 방문자 로그 자동 정리(retention) 정책: **데이터 삭제 없이 집계+아카이브로 테이블 크기를 관리하는 설계**를 별도 문서로 남김 — [`2026-08-14-counter-log-archive-design.md`](./2026-08-14-counter-log-archive-design.md). 이번 InnoDB 전환과는 독립적으로, 여유를 갖고 별도 진행.
- mysqldump가 매번 이런 대형 로그 테이블을 통째로 백업하는 것 자체도 백업 시간·부하의 원인이므로, 정리/InnoDB 전환 이후에도 백업 전략(예: 로그성 테이블 백업 주기 분리) 재검토 여지 있음.
