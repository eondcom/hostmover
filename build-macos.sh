#!/usr/bin/env bash
# Hostmover macOS 빌드/패키징 스크립트.
#   ⚠️ 반드시 macOS 에서 실행해야 한다 (eframe/egui 는 실질적으로 크로스컴파일 불가).
#
#   ./build-macos.sh            # 현재 아키텍처용 .app 번들 생성
#   ./build-macos.sh --universal# x86_64 + arm64 유니버설 바이너리
#   ./build-macos.sh --dmg      # .app + 배포용 .dmg 생성
#
# 산출물: dist/Hostmover.app  (그리고 --dmg 시 dist/Hostmover.dmg)
set -euo pipefail
cd "$(dirname "$0")"

APP_NAME="Hostmover"
BUNDLE_ID="com.eond.hostmover"
VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
DIST="dist"
APP="$DIST/$APP_NAME.app"

if [[ "$(uname)" != "Darwin" ]]; then
  echo "✗ 이 스크립트는 macOS 에서 실행해야 합니다 (현재: $(uname))." >&2
  echo "  Linux/Windows 에서는 egui 앱을 macOS 용으로 크로스컴파일할 수 없습니다." >&2
  exit 1
fi

UNIVERSAL=0
MAKE_DMG=0
for a in "$@"; do
  case "$a" in
    --universal) UNIVERSAL=1 ;;
    --dmg) MAKE_DMG=1 ;;
    *) echo "알 수 없는 옵션: $a" >&2; exit 1 ;;
  esac
done

echo "▶ Hostmover $VERSION 빌드 중…"
if [[ "$UNIVERSAL" == "1" ]]; then
  rustup target add x86_64-apple-darwin aarch64-apple-darwin >/dev/null 2>&1 || true
  cargo build --release --target x86_64-apple-darwin
  cargo build --release --target aarch64-apple-darwin
  BIN="$DIST/hostmover-universal"
  mkdir -p "$DIST"
  lipo -create -output "$BIN" \
    target/x86_64-apple-darwin/release/hostmover \
    target/aarch64-apple-darwin/release/hostmover
else
  cargo build --release
  BIN="target/release/hostmover"
fi

# ── .app 골격 ───────────────────────────────────────────────
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/$APP_NAME"
chmod +x "$APP/Contents/MacOS/$APP_NAME"

# ── 아이콘 (assets/hostmover.png → .icns) ────────────────────
if [[ -f assets/hostmover.png ]] && command -v iconutil >/dev/null 2>&1; then
  ICONSET="$DIST/Hostmover.iconset"
  rm -rf "$ICONSET"; mkdir -p "$ICONSET"
  for s in 16 32 64 128 256 512; do
    sips -z $s $s assets/hostmover.png --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
    d=$((s*2))
    sips -z $d $d assets/hostmover.png --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
  done
  iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/$APP_NAME.icns"
  rm -rf "$ICONSET"
  ICON_LINE="<key>CFBundleIconFile</key><string>$APP_NAME</string>"
else
  echo "  (아이콘 생략: assets/hostmover.png 또는 iconutil 없음)"
  ICON_LINE=""
fi

# ── Info.plist ──────────────────────────────────────────────
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>$APP_NAME</string>
  <key>CFBundleDisplayName</key><string>$APP_NAME</string>
  <key>CFBundleExecutable</key><string>$APP_NAME</string>
  <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>10.15</string>
  <key>NSHighResolutionCapable</key><true/>
  $ICON_LINE
</dict>
</plist>
PLIST

# ── ad-hoc 코드서명 (Gatekeeper 1차 통과; 정식 배포는 Developer ID 필요) ──
codesign --force --deep --sign - "$APP" >/dev/null 2>&1 \
  && echo "  ad-hoc 서명 완료" || echo "  (codesign 생략)"

echo "✓ 생성: $APP"

# ── DMG ─────────────────────────────────────────────────────
if [[ "$MAKE_DMG" == "1" ]]; then
  DMG="$DIST/$APP_NAME.dmg"
  rm -f "$DMG"
  STAGE="$DIST/dmg-stage"; rm -rf "$STAGE"; mkdir -p "$STAGE"
  cp -R "$APP" "$STAGE/"
  ln -s /Applications "$STAGE/Applications"
  hdiutil create -volname "$APP_NAME" -srcfolder "$STAGE" -ov -format UDZO "$DMG" >/dev/null
  rm -rf "$STAGE"
  echo "✓ 생성: $DMG"
fi

echo
echo "실행: open $APP"
echo "처음 실행 시 Gatekeeper 차단되면: 우클릭 → 열기, 또는"
echo "  xattr -dr com.apple.quarantine \"$APP\""
