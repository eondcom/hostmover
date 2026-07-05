# Hestia 백업 실패 인시던트 — `/backup` ext4 파일시스템 손상

- **서버**: mars.eond.com
- **최초 증상 발생**: 2026-06-24 05:57 (백업 cron)
- **진단/복구**: 2026-06-26
- **최초 추정**: ext4 논리 손상(`e2fsck`로 복구 가능) — **오진. 아래로 정정.**
- **최종 결론**: **하드웨어 결함 — 백업 디스크 `/dev/sda`(3.6T HDD)가 부하 시 버스에서 탈락.** ext4 손상은 죽어가는 디스크가 만든 *증상*이었음. 소프트웨어(fsck/mkfs)로 복구 불가. **통큰서버에 물리 점검·교체 요청 필요.** 라이브 데이터는 별개 디스크(nvme, `/home`)에 정상.

> ### ⚠️ 진단이 뒤집힌 지점 (핵심 교훈)
> `smartctl` PASSED + `EXT4-fs error ... checksum`만 보고 "디스크 정상 / 논리 손상"으로 판단했으나, `e2fsck`가 **고친 메타데이터가 다시 깨지며 무한 루프**(수정이 디스크에 안 박힘) → 이어서 `Error writing block ... (Bad file descriptor)` / `/dev/sda1: Can't open blockdev` → `mount` 시 `by-uuid 장치 없음`. **소량 I/O는 되지만 지속 부하에서 장치가 탈락**하는 패턴이 진짜 원인. SMART PASSED는 "읽을 수 있을 때 찍은 값"일 뿐 하드웨어 정상을 보장하지 않는다.
>
> ### 🔴 결정적 증거 (논쟁 종료)
> 디스크 재인식 후 `dd`로 20GB 읽기는 239MB/s로 통과 → "읽기 정상". 그러나 **`mkfs.ext4`가 성공적으로 끝난 직후 `e2fsck -fn`이 그 새 파일시스템에서 즉시 손상 검출**(`Corrupt group descriptor`, `Bad magic number in super-block`). **갓 포맷한 fs가 읽자마자 깨짐 = 디스크가 쓴 데이터를 정상 저장하지 못함.** 읽기 통과는 "쓴 게 맞는지"를 검증하지 못한다 — 검증은 write→read-back 비교라야 한다.
>
> ### 🔬 정밀 진단: 배드섹터 아님 → silent corruption
> dmesg 전수 검사 결과 **저수준 블록 I/O 에러가 전무**하다: `ata reset` / `blk_update_request I/O error` / `Medium Error` / SMART pending sector **모두 0건**. 잡힌 것은 전부 ext4 레벨의 `bad block bitmap checksum` / `Filesystem failed CRC` / `Delayed block allocation failed ... error 74(EBADMSG)·117(EUCLEAN)`. `error count since last fsck: 2484`. → **배드섹터(디스크 노화)가 아니라, 쓰기는 ack되는데 읽으면 내용이 바뀌어 있는 silent data corruption.** 손상이 `/dev/sda`에만 국한(`/`·`/home` nvme 정상)되므로 원인은 **sda 경로 하드웨어**(디스크 펌웨어/캐시, SATA 케이블·포트, 컨트롤러 채널)로 좁혀진다. RAM/CPU였다면 타 디스크도 손상됐을 것. → **디스크 교체 + 케이블·포트 점검.** `e2fsck`/`mkfs`는 무의미(고치는 속도보다 깨지는 속도가 빠름 — 2484 에러가 증명).

---

## 1. 증상

HestiaCP에서 백업 실패 메일 수신:

```
oracall → backup failed
Hestia Control Panel <noreply@mars.eond.com>
Can't create tmp dir
```

- 에러 메시지: 백업 스크립트가 `mktemp -p /backup -d`로 임시 작업 폴더를 만들다 실패.

**실패 타임라인 (실패 메일 기준)** — 날마다 실패 유저가 바뀜:

| 날짜 | 실패 유저 |
|---|---|
| 6/24 | britzhys, oracall |
| 6/25 | hani, admin, yncare, oracall, dive |

→ 특정 유저 문제가 아니라 **손상이 파일시스템 전반에 흩어져 있어서**, 그날 임시 폴더(`mktemp`)가 손상 구간에 잡히는 유저만 실패하는 패턴. 유저가 매일 바뀌는 것 자체가 손상이 광범위하다는 증거이며, `e2fsck`로 비트맵을 통째로 재배치해 한 번에 해결됨(유저별 개별 조치 불필요).

## 2. 진단 과정 (단계별 배제)

### 2-1. 단순 원인 배제 — 디스크/권한/마운트 전부 정상

```bash
df -h /backup          # 31% 사용, 2.4T 여유  → 디스크 풀 아님
df -i /backup          # inode 1% 사용         → inode 고갈 아님
mount | grep backup    # /dev/sda1 ext4 (rw)   → 읽기전용/언마운트 아님
touch /backup/_writetest && rm /backup/_writetest   # 쓰기 OK → 권한 정상
```

→ "디스크 꽉 참" 같은 흔한 원인은 전부 아님.

### 2-2. 범인 식별 — 실패가 남긴 임시 디렉터리

```bash
ls -la /backup
# drwx------ 4 root root 4096 Jun 24 05:56 tmp.OUcEBFKOAI   ← 6/24 실패 잔재(메일 시각과 일치)
# drwx------ 4 innostv root ... Dec 16 2025 tmp.Grt1876cMp  ← 작년 12월 실패 잔재
# drwx------ 4 costoms root ... Dec 16 2025 tmp.jdunU63q4f  ← 작년 12월 실패 잔재
```

백업 도중 죽으면 임시 폴더만 남는다. 6/24 05:56 생성된 `tmp.OUcEBFKOAI`가 실패 메일(05:59)의 잔재.

### 2-3. 결정적 증거 — `Bad message` (EBADMSG)

```bash
du -sh /backup/tmp.*
# du: cannot read directory '/backup/tmp.Grt1876cMp/pam': Bad message
rm -rf /backup/tmp.*
# rm: cannot remove '.../pam': Directory not empty   ← 비었는데 못 지움
```

`Bad message` = `EBADMSG`. ext4(`metadata_csum`)에서 **디렉터리/비트맵 블록 체크섬이 깨졌을 때** 나는 에러. 읽지도 지우지도 못함 → 파일시스템 손상 확정.

### 2-4. 손상 vs 디스크 수명 판별

**커널 로그 (`dmesg`)** — ext4 메타데이터 체크섬 오류 확인:

```
EXT4-fs error (device sda1): ext4_validate_block_bitmap: bad block bitmap checksum
EXT4-fs error (device sda1) in ext4_mb_clear_bb: Filesystem failed CRC
EXT4-fs error (device sda1): htree_dirblock_to_tree: Directory block failed checksum
EXT4-fs warning (device sda1): ... Please run e2fsck -D.
EXT4-fs (sda1): Delayed block allocation failed for inode 21 ... error 117
EXT4-fs (sda1): This should not happen!! Data will be lost   ← 마운트 중 쓰기마다 손상 확산
```

**SMART (`smartctl -a /dev/sda`)** — 물리 디스크는 건강:

```
SMART overall-health self-assessment test result: PASSED
  5 Reallocated_Sector_Ct      0
197 Current_Pending_Sector     0
198 Offline_Uncorrectable      0
  9 Power_On_Hours         18259   (~2년, 정상)
```

→ **디스크 하드웨어 정상 + ext4 메타데이터 체크섬 손상** = `fsck`로 고치는 **논리적 손상**. 디스크 교체 불필요. 과거 비정상 종료/정전/커널 사고로 블록 비트맵·디렉터리 체크섬이 깨진 전형적 패턴.

## 3. 근본 원인

`/dev/sda1`(`/backup`, ext4, 3.6T)의 **블록 비트맵 / 디렉터리 블록 체크섬 손상**.

- 백업이 `mktemp -p /backup`로 임시 폴더를 만들다 손상 구간을 밟으면 실패 → `Can't create tmp dir`.
- 손상 구간을 안 밟은 날/유저는 성공 → 간헐적 실패로 보임.
- **마운트된 상태로 계속 쓰면 손상이 확산**(`error 117` / "Data will be lost")되는 진행형 상태였음.

> 라이브 사이트 데이터는 `/home`(nvme0n1p1, 별개 디스크, 정상)에 있고 `/dev/sda1`은 백업 저장용일 뿐. 최악의 경우에도 옛 백업 일부만 손실, 라이브 서비스 영향 없음.

## 4. 복구 절차

```bash
# 1) /backup 쓰기 즉시 중단 (야간 백업 cron)
systemctl stop cron

# 2) 언마운트 (busy면 잡은 프로세스 종료 후 재시도)
fuser -vmk /backup
umount /backup

# 3) 강제 점검+복구 (-f 강제, -y 자동yes, -D 디렉터리 재구성: dmesg가 명시 요청)
e2fsck -fyD /dev/sda1
#  → "Relocating group N's block bitmap/inode bitmap/inode table" 단계는
#     깨진 비트맵을 재배치하는 정상 복구. 절대 중단 금지(Ctrl-C/터미널 닫기 금지).
#  → 3.6T라 수십 분~몇 시간 소요. SSH 끊김 주의(절전/세션 종료 금지).
#  → 끝에 "FILE SYSTEM WAS MODIFIED" 뜨면 완료.
#  → 추가 에러가 남으면 깨끗해질 때까지 `e2fsck -fy /dev/sda1` 반복.

# 4) 재마운트 + 검증
mount /backup
dmesg -T | tail -20        # 새 EXT4-fs error 없어야 정상
ls -la /backup/tmp.*       # 이제 읽히고 지워져야 함
ls -la /backup/lost+found  # fsck가 건진 조각

# 5) cron 재개
systemctl start cron
```

## 5. 복구 후 정리

```bash
# 1) 남은 임시 폴더 제거 (이제 정상 삭제됨)
rm -rf /backup/tmp.*

# 2) 깨끗한 백업 새로 확보
v-backup-users

# 3) 고아 백업(삭제된 유저의 .tar) 정리 — 실유저와 대조
comm -23 \
  <(ls /backup/*.tar | sed 's#.*/##; s/\..*//' | sort -u) \
  <(v-list-users | awk 'NR>2{print $1}' | sort -u)
#  여기 뜨는 prefix = 지워도 되는 고아 백업
#  예: teacher(113G·유저삭제됨), watch, bluewings, ohcanada, nolazzo,
#      koreanhub, sohee, cimin, itmang, poyo, ihi, neosol_sclink.co.kr_*.zip 등
```

## 6. 재발 방지 / 후속 점검

- **정전 대비(UPS)** — 논리 손상의 가장 흔한 원인이 비정상 종료. UPS + 정상 셧다운 보장.
- **주기적 fsck** — 마운트 카운트/시간 기반 자동 점검 설정:
  ```bash
  tune2fs -c 30 -i 30d /dev/sda1   # 30회 마운트 또는 30일마다 부팅 시 점검
  ```
- **SMART 모니터링** — `smartd`(설치됨) 활성화로 디스크 이상 조기 경보.
- **백업 무결성 확인** — 손상 기간(6/24~6/26)에 쓰인 .tar는 손상 가능성. 핵심 유저 백업은 복원 테스트 권장:
  ```bash
  tar -tf /backup/<user>.<date>.tar >/dev/null && echo OK
  ```
- **이중화 고려** — `/backup`이 단일 디스크. 중요 백업은 원격/오프사이트로 한 벌 더(rsync, S3 등).

### 🛠 hostmover 내장 도구 (이 인시던트를 계기로 추가, 2026-06-29)

설정 → **디스크 점검** 탭:

- **디스크 건강 진단**(읽기 전용): `dmesg` I/O·ext4 체크섬 에러, `tune2fs` FS상태/누적 에러카운트(여기서 "error count 2484"가 잡힘), SMART, RO 재마운트, 용량을 한 번에 수집.
- **무결성 검사(write→read-back)**: SMART/읽기테스트가 못 잡는 silent corruption 확정용. 임시파일 쓰기→캐시드롭→재독 해시 비교. **교체될 새 디스크도 신뢰 전 이 검사 1회 권장.**
- **자동 감시 설치**: smartd + 매일 FS에러카운트/dmesg/SMART/용량 감시 + 주간 스크럽 + 이상 시 이메일(eond@eond.com). 서버에 `/usr/local/sbin/hm-disk-monitor.sh`·`hm-disk-scrub.sh` + `/etc/cron.d/hm-disk-monitor` 설치, 로그 `/var/log/hm-disk-monitor.log`. 설치 마지막에 **테스트 메일을 실제로 1통 발송**하고 사용된 전송수단(mail/sendmail)을 로그에 표시 → 알림 경로를 즉시 검증. 모든 감시 스크립트의 PATH에 `/usr/sbin:/sbin` 포함(비대화형 SSH에서도 exim `sendmail`·`tune2fs`·`smartctl`을 확실히 찾도록).

> 핵심 교훈의 코드화: **읽기 테스트로는 silent corruption을 못 잡는다 → write→read-back 이 유일한 확정 판별.** FS 에러카운트 증가 추적이 가장 싼 조기경보.

---

## 부록: 핵심 교훈

| 증상 | 흔한 오해 | 실제 원인 |
|---|---|---|
| `Can't create tmp dir` | 디스크 풀 / 권한 | ext4 메타데이터 손상으로 `mktemp` 실패 |
| `Bad message` on `ls`/`rm` | 권한 문제 | `EBADMSG` = 체크섬 손상 |
| 간헐적 실패 | 메모리/타이밍 | 손상 구간을 밟느냐 마느냐의 차이 |

`dmesg`의 `EXT4-fs error ... checksum` + `smartctl` PASSED 조합 = **하드웨어 정상, fsck로 해결되는 논리 손상**. 이 두 출력이 진단의 핵심.
