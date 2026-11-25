#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 XingLai AI and Concat contributors
#
# 星籁剪辑 (XingLai Cut) 的 macOS 打包脚本：把一个已经编译好的二进制做成
# `.app`，签好名，再打成 `.dmg`。
#
#   scripts/macos/package.sh [--target aarch64-apple-darwin|x86_64-apple-darwin]
#                            [--version 0.5.0] [--out DIR] [--app-name 星籁剪辑]
#                            [--identity "Developer ID Application: ..."]
#                            [--notarize]
#
# 本机先编好二进制（在 src/ 里）：
#   cargo build --profile app -p concat --target aarch64-apple-darwin
#
# 前置：
#   - FFmpeg 开发库。macOS 上用 src/scripts/ffmpeg-macos.sh 从源码编 LGPL
#     的共享库，脚本会把 FFMPEG_DIR 写进环境；见 build-app.yml 同名步骤。
#   - `brew install dylibbundler`：把 FFmpeg 的 dylib 拷进 .app 并改加载路径。
#   - Apple Developer Program 账号（¥688/年）才能签名 + 公证：
#       https://developer.apple.com/programs/
#     没有证书时本脚本做 ad-hoc 签名：包能打开，但 Gatekeeper 会提示
#     「未识别的开发者」，企业客户分发前必须补上证书。
#
# 签名 / 公证要用的环境变量（或 --identity 传身份）：
#   CODESIGN_IDENTITY="Developer ID Application: XingLai (TEAMID)"
#   APPLE_ID=you@example.com APPLE_TEAM_ID=TEAMID APPLE_APP_PASSWORD=xxxx-xxxx-xxxx-xxxx
#   scripts/macos/package.sh --notarize
#
# 输出：默认写到 src/target/macos-<arch>/，最后打印两行，脚本和 CI 都读它：
#   app=<...>.app
#   dmg=<...>.dmg
set -euo pipefail

say() { printf '\n== %s\n' "$*"; }
note() { printf '%s\n' "$*" >&2; }
die() { printf 'package-macos: %s\n' "$*" >&2; exit 1; }

usage() {
  cat <<'USAGE'
Builds the macOS distribution of 星籁剪辑 (XingLai Cut): a .app bundle and a
.dmg, signed and notarised when Apple credentials are at hand.

  scripts/macos/package.sh [--target aarch64-apple-darwin|x86_64-apple-darwin]
                           [--version 0.5.0] [--out DIR] [--app-name 星籁剪辑]
                           [--identity "Developer ID Application: ..."]
                           [--notarize]

Build the binary first (from src/):
  cargo build --profile app -p concat --target aarch64-apple-darwin

Needs: FFmpeg dev libraries from src/scripts/ffmpeg-macos.sh (FFMPEG_DIR),
`brew install dylibbundler`, and - for signing and notarisation - an Apple
Developer Program membership (¥688/year). Without one the bundle is ad-hoc
signed and Gatekeeper warns on first open.

Environment for signing: CODESIGN_IDENTITY, APPLE_ID, APPLE_TEAM_ID,
APPLE_APP_PASSWORD. Optional: FFMPEG_CLI_DIR (a directory with ffmpeg and
ffprobe), ORT_MAC_DYLIB (Intel Mac's ONNX Runtime), MACOSX_DEPLOYMENT_TARGET.
USAGE
}

TARGET=""
VERSION=""
OUT=""
APP_NAME="${APP_NAME:-星籁剪辑}"
IDENTITY="${CODESIGN_IDENTITY:-}"
NOTARIZE=0
while [ $# -gt 0 ]; do
  case "$1" in
    --target) TARGET="${2:-}"; shift 2 ;;
    --version) VERSION="${2:-}"; shift 2 ;;
    --out) OUT="${2:-}"; shift 2 ;;
    --app-name) APP_NAME="${2:-}"; shift 2 ;;
    --identity) IDENTITY="${2:-}"; shift 2 ;;
    --notarize) NOTARIZE=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown argument: $1 (try --help)" ;;
  esac
done

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
src="$root/src"
[ -f "$src/Cargo.toml" ] || die "the Cargo workspace is not at $src"

# Which machine this bundle is for. No target means the machine running this.
if [ -z "$TARGET" ]; then
  case "$(uname -m)" in
    arm64) TARGET=aarch64-apple-darwin ;;
    x86_64) TARGET=x86_64-apple-darwin ;;
    *) die "unknown host architecture: $(uname -m)" ;;
  esac
fi
case "$TARGET" in
  aarch64-apple-darwin) ARCH=arm64 ;;
  x86_64-apple-darwin) ARCH=x86_64 ;;
  *) die "not a macOS target: $TARGET" ;;
esac

# The version is the workspace's, the same one Cargo puts in the binary.
if [ -z "$VERSION" ]; then
  VERSION=$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' "$src/Cargo.toml" | head -1)
fi
[ -n "$VERSION" ] || die "cannot read the workspace version from $src/Cargo.toml"

profile="${PROFILE:-app}"
bin="$src/target/$TARGET/$profile/concat"
[ -x "$bin" ] || die "no binary at $bin — run: (cd $src && cargo build --profile $profile -p concat --target $TARGET)"
file -b "$bin" | grep -q 'Mach-O' || die "$bin is not a Mach-O binary (wrong --target?)"

MIN_MACOS="${MACOSX_DEPLOYMENT_TARGET:-12.0}"
BUNDLE_ID="${BUNDLE_ID:-com.xinglaiai.xinglaicut}"
OUT="${OUT:-$src/target/macos-$ARCH}"
APP="$OUT/$APP_NAME.app"
DMG="$OUT/$APP_NAME-$VERSION-macos-$ARCH.dmg"

say "bundle"
note "target $TARGET, version $VERSION, minimum macOS $MIN_MACOS"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources/bin" \
         "$APP/Contents/Resources/legal" "$APP/Contents/Frameworks"
cp "$bin" "$APP/Contents/MacOS/concat"

# Apple's icon: an .icns made from the 1024px PNG's sizes. sips and iconutil
# are part of the system; nothing else is needed to draw the app's icon.
iconset="$OUT/AppIcon.iconset"
rm -rf "$iconset"
mkdir -p "$iconset"
icon="$root/assets/icons/xinglai-1024.png"
[ -f "$icon" ] || icon="$root/assets/icons/concat_logo_512.png"
[ -f "$icon" ] || die "no icon source under assets/icons"
for size in 16 32 64 128 256 512 1024; do
  sips -z "$size" "$size" "$icon" --out "$iconset/icon_${size}x${size}.png" >/dev/null
done
cp "$iconset/icon_32x32.png" "$iconset/icon_16x16@2x.png"
cp "$iconset/icon_64x64.png" "$iconset/icon_32x32@2x.png"
cp "$iconset/icon_256x256.png" "$iconset/icon_128x128@2x.png"
cp "$iconset/icon_512x512.png" "$iconset/icon_256x256@2x.png"
cp "$iconset/icon_1024x1024.png" "$iconset/icon_512x512@2x.png"
# The two sizes only needed as @2x twins are not part of the iconset's names.
rm -f "$iconset/icon_64x64.png" "$iconset/icon_1024x1024.png"
iconutil -c icns "$iconset" -o "$APP/Contents/Resources/AppIcon.icns"
rm -rf "$iconset"

# AGPL 的源码义务随包走：LICENSE 与第三方声明、源码要约、AI 插件清单。
for doc in LICENSE THIRD_PARTY_NOTICES.md SOURCE_OFFER.md AI_PLUGINS_NOTICES.md LICENSING.md; do
  if [ -f "$root/$doc" ]; then
    cp "$root/$doc" "$APP/Contents/Resources/legal/"
  fi
done

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>星籁剪辑</string>
  <key>CFBundleDisplayName</key><string>星籁剪辑 XingLai Cut</string>
  <key>CFBundleIdentifier</key><string>${BUNDLE_ID}</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleExecutable</key><string>concat</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleDevelopmentRegion</key><string>zh_CN</string>
  <key>LSMinimumSystemVersion</key><string>${MIN_MACOS}</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.video</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
  <key>NSMicrophoneUsageDescription</key><string>星籁剪辑需要麦克风来录制解说配音。</string>
  <key>NSHumanReadableCopyright</key><string>AGPL-3.0-or-later. 星籁剪辑 (c) 2026 XingLai. Based on Concat, (c) 2026 Jareer Abdullah. See THIRD_PARTY_NOTICES.md.</string>
</dict>
</plist>
PLIST
plutil -lint "$APP/Contents/Info.plist" >/dev/null
say "info.plist ok"

# 随包的 ffmpeg / ffprobe 命令行：悬停预览、素材指纹和本地 AI 的成片拼接
# 都要起它们。macOS 的 FFmpeg 开发库（src/scripts/ffmpeg-macos.sh）默认
# 只编库不编命令行程序，所以这里允许用 FFMPEG_CLI_DIR 指一个单独编好的
# LGPL 版本；没有就只发库，那几项功能在 Mac 上先不可用（见移植障碍清单）。
cli_dir="${FFMPEG_CLI_DIR:-$src/vendor/ffmpeg/macos/bin}"
if [ -x "$cli_dir/ffmpeg" ] && [ -x "$cli_dir/ffprobe" ]; then
  cp "$cli_dir/ffmpeg" "$cli_dir/ffprobe" "$APP/Contents/Resources/bin/"
  note "bundled the command-line ffmpeg and ffprobe from $cli_dir"
elif [ -n "${FFMPEG_CLI_DIR:-}" ]; then
  die "FFMPEG_CLI_DIR=$cli_dir has no executable ffmpeg and ffprobe"
else
  note "warning: no ffmpeg/ffprobe command-line tools at $cli_dir."
  note "         Hover previews, media fingerprints and local AI muxing need them"
  note "         on macOS; build an LGPL pair and pass FFMPEG_CLI_DIR. See"
  note "         docs/macos-port/01-移植障碍清单.md."
fi

# Every dylib the binary (and those dylibs) load, copied into Frameworks and
# re-pointed at the bundle: a .dmg must run on a Mac with no Homebrew.
command -v dylibbundler >/dev/null 2>&1 \
  || die "dylibbundler is missing: brew install dylibbundler"
say "libraries"
dylibbundler -od -b \
  -x "$APP/Contents/MacOS/concat" \
  -d "$APP/Contents/Frameworks" \
  -p @executable_path/../Frameworks

# Intel Mac's ONNX Runtime is loaded at first use rather than linked (it is
# built for macOS 13.4 while the app runs on 12); see concat-vision's manifest.
lazy=()
if [ -n "${ORT_MAC_DYLIB:-}" ]; then
  [ -f "$ORT_MAC_DYLIB" ] || die "ORT_MAC_DYLIB=$ORT_MAC_DYLIB is not a file"
  cp "$ORT_MAC_DYLIB" "$APP/Contents/Frameworks/libonnxruntime.dylib"
  lazy=(--lazy Contents/Frameworks/libonnxruntime.dylib)
fi

# Nothing may load from outside the bundle, and every library must run on the
# minimum macOS the Info.plist declares. check-min-os.sh is shared with CI.
if otool -L "$APP/Contents/MacOS/concat" | grep -E '/opt/homebrew|/usr/local' ; then
  die "the binary still loads libraries from outside the bundle"
fi
if [ -f "$root/scripts/ci/check-min-os.sh" ]; then
  bash "$root/scripts/ci/check-min-os.sh" "$APP" "$MIN_MACOS" ${lazy[@]+"${lazy[@]}"}
fi

# Signing: Developer ID + hardened runtime + the microphone entitlement when an
# identity is given, ad-hoc otherwise (which arm64 requires anyway, so a bare
# build opens with Gatekeeper's warning instead of being killed).
entitlements="$OUT/xinglai.entitlements"
cat > "$entitlements" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>com.apple.security.device.audio-input</key><true/></dict></plist>
PLIST
say "codesign"
if [ -n "$IDENTITY" ]; then
  note "signing as: $IDENTITY"
  find "$APP/Contents/Frameworks" -type f -name '*.dylib' -exec \
    codesign --force --options runtime --timestamp -s "$IDENTITY" {} \;
  codesign --force --options runtime --timestamp --entitlements "$entitlements" \
    -s "$IDENTITY" "$APP/Contents/MacOS/concat"
  codesign --force --options runtime --timestamp --entitlements "$entitlements" \
    -s "$IDENTITY" "$APP"
  codesign --verify --deep --strict "$APP"
else
  note "no CODESIGN_IDENTITY/--identity: ad-hoc signing."
  note "For a distributable build set a Developer ID identity; the"
  note "Apple Developer Program is ¥688/year."
  codesign --force --deep -s - "$APP"
fi

say "dmg"
hdiutil create -volname "$APP_NAME" -srcfolder "$APP" -ov -format UDZO "$DMG" >/dev/null

if [ "$NOTARIZE" -eq 1 ]; then
  [ -n "$IDENTITY" ] || die "--notarize needs a signing identity (--identity or CODESIGN_IDENTITY)"
  [ -n "${APPLE_ID:-}" ] || die "--notarize needs APPLE_ID"
  [ -n "${APPLE_TEAM_ID:-}" ] || die "--notarize needs APPLE_TEAM_ID"
  [ -n "${APPLE_APP_PASSWORD:-}" ] || die "--notarize needs APPLE_APP_PASSWORD"
  say "notarise"
  codesign --force --timestamp -s "$IDENTITY" "$DMG"
  xcrun notarytool submit "$DMG" --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" \
    --password "$APPLE_APP_PASSWORD" --wait
  xcrun stapler staple "$DMG"
  xcrun stapler validate "$DMG"
else
  note ""
  note "Not notarised. Apple's Gatekeeper warns on the first open of a build"
  note "without a Developer ID and notarisation. Re-run with --notarize once"
  note "APPLE_ID / APPLE_TEAM_ID / APPLE_APP_PASSWORD are set; the Apple"
  note "Developer Program is ¥688/year (docs/macos-port/02-移植计划.md)."
fi

say "done"
note "$APP"
note "$DMG"
printf 'app=%s\n' "$APP"
printf 'dmg=%s\n' "$DMG"
