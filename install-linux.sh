#!/usr/bin/env bash
# Hostmover Linux 설치 스크립트 — 빌드한 것이 '런처/독바에서 실제로 뜨는 앱'이 되게 한다.
#
#   ./install-linux.sh              # 릴리스 빌드 후 설치
#   ./install-linux.sh --no-build   # 이미 빌드된 target/release 바이너리만 설치
#
# worktree 안에서 실행해도 된다. 오히려 그게 정상 경로다 —
# 산출물은 worktree 안에 생기지만, 설치는 항상 ~/.local/bin 으로 간다.
#
# 왜 이 스크립트가 있나 (2026-08-05 실제 사고 두 건):
#   ① 런처가 target/release 를 직접 가리키고 있었는데 cargo clean 으로 그 파일이 사라져,
#      아이콘을 눌러도 아무 반응이 없었다 (Terminal=false 라 에러도 안 뜬다).
#   ② ~/.local/bin 으로 옮긴 뒤에도 COSMIC 독바가 옛 Exec 을 캐시하고 있어
#      옛 바이너리가 계속 떴다. 그래서 아래 ⑤ 에서 옛 경로도 같이 덮는다.
set -euo pipefail
cd "$(dirname "$0")"

BIN_DIR="$HOME/.local/bin"
BIN="$BIN_DIR/hostmover"
APP_DIR="$HOME/.local/share/applications"
DESKTOP="$APP_DIR/hostmover.desktop"
ICON_DIR="$HOME/.local/share/icons/hicolor/256x256/apps"

DO_BUILD=1
for a in "$@"; do
  case "$a" in
    --no-build) DO_BUILD=0 ;;
    -h|--help) sed -n '2,9p' "$0"; exit 0 ;;
    *) echo "✗ 알 수 없는 옵션: $a" >&2; exit 1 ;;
  esac
done

# ① 빌드 (릴리스만 — 런처는 debug 를 보지 않는다)
if [[ $DO_BUILD == 1 ]]; then
  echo "== 릴리스 빌드 =="
  cargo build --release
fi
SRC="target/release/hostmover"
[[ -x "$SRC" ]] || { echo "✗ $SRC 없음 — --no-build 없이 다시 실행하세요." >&2; exit 1; }

# ② 바이너리 설치 (복사이므로 cargo clean 에 영향받지 않는다)
install -Dm755 "$SRC" "$BIN"
echo "설치: $BIN"

# ③ 아이콘
[[ -f assets/hostmover.png ]] && install -Dm644 assets/hostmover.png "$ICON_DIR/hostmover.png"

# ④ 런처 항목 — Exec 은 반드시 ~/.local/bin (빌드 산출물 경로를 직접 가리키지 말 것)
mkdir -p "$APP_DIR"
cat > "$DESKTOP" <<EOF
[Desktop Entry]
Type=Application
Version=1.0
Name=Hostmover
GenericName=Hosting Migrator
Comment=호스팅 이전 백업/복원 도구
Exec=$BIN
Icon=hostmover
Terminal=false
Categories=Network;
Keywords=hosting;migration;backup;rsync;mysqldump;호스팅;이전;
StartupNotify=true
StartupWMClass=hostmover
EOF
command -v update-desktop-database >/dev/null && update-desktop-database "$APP_DIR" || true
echo "런처: $DESKTOP"

# ⑤ 독바 캐시 대비 — 옛 Exec(메인 체크아웃의 target/release)이 남아 있으면 같이 덮는다.
#    COSMIC 독바는 .desktop 의 Exec 을 캐시해서, 고쳐도 한동안 옛 경로로 실행한다.
#    그 경로가 새 바이너리면 캐시가 남아 있어도 결과는 같다.
MAIN_ROOT="$(dirname "$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null || echo /nonexistent/.git)")"
LEGACY="$MAIN_ROOT/target/release/hostmover"
if [[ -f "$LEGACY" && "$LEGACY" != "$(readlink -f "$SRC")" ]]; then
  install -m755 "$BIN" "$LEGACY"
  echo "옛 경로도 갱신: $LEGACY  (독바 캐시 대비)"
fi

# ⑥ 확인
echo
echo "== 설치된 버전 =="
strings "$BIN" | grep -oE '\b[0-9a-f]{7}(\+수정본)?\b' | head -1 || true
echo "  (git HEAD = $(git rev-parse --short HEAD 2>/dev/null || echo '?'))"
if pgrep -x hostmover >/dev/null; then
  echo
  echo "※ Hostmover 가 실행 중입니다 — 껐다 켜야 새 버전이 뜹니다."
  echo "   앱 상단바의 버전 문자열이 위 해시와 같은지 확인하세요."
fi
