#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Builds "AV Relay.app" (one binary for Apple silicon and Intel), signs it,
# and packs it as a .zip and a .dmg.
#
#   scripts/build-app.sh [--version 1.2.3] [--out DIR] [--native]
#
# --native builds for this Mac's architecture only (faster, for testing).
#
# Signing is ad hoc unless AVRELAY_SIGN_IDENTITY names a code-signing
# identity of yours (a self-signed one is fine). A stable signature keeps
# macOS's camera and microphone permission across rebuilds; an ad hoc one
# changes with every build. If the identity lives in a keychain of its own:
# AVRELAY_SIGN_KEYCHAIN=path and AVRELAY_SIGN_KEYCHAIN_PASSWORD_FILE=file
# unlock it first.
set -euo pipefail
cd "$(dirname "$0")/.."

version="0.0.0-dev"
out="build"
archs="arm64 x86_64"
while (($#)); do
	case $1 in
	--version) version=$2; shift 2 ;;
	--out) out=$2; shift 2 ;;
	--native) archs=""; shift ;;
	*) echo "usage: $0 [--version X.Y.Z] [--out DIR] [--native]" >&2; exit 2 ;;
	esac
done
build=$(git rev-list --count HEAD 2>/dev/null || echo 1)
mkdir -p "$out"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# One architecture per build, then lipo: building both at once switches some
# Xcode versions to a build system that loses libsodium's prebuilt library.
if [[ -z $archs ]]; then
	swift build -c release --product AVRelay
	bin=$(swift build -c release --product AVRelay --show-bin-path)
	cp "$bin/AVRelay" "$work/AVRelay"
else
	slices=()
	for arch in $archs; do
		swift build -c release --product AVRelay --arch "$arch"
		bin=$(swift build -c release --product AVRelay --arch "$arch" --show-bin-path)
		cp "$bin/AVRelay" "$work/AVRelay-$arch"
		slices+=("$work/AVRelay-$arch")
	done
	lipo -create -output "$work/AVRelay" "${slices[@]}"
fi
# WebRTC.framework comes prebuilt for both architectures.
lipo -info "$work/AVRelay"

app="$out/AV Relay.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Frameworks" "$app/Contents/Resources"
cp "$work/AVRelay" "$app/Contents/MacOS/AVRelay"
ditto "$bin/WebRTC.framework" "$app/Contents/Frameworks/WebRTC.framework"
sed -e "s/@VERSION@/$version/g" -e "s/@BUILD@/$build/g" App/Info.plist >"$app/Contents/Info.plist"
# The logo (assets/logo/generate.py): the app icon and the menu bar's template images.
icons=../../assets/generated
cp "$icons/AppIcon.icns" "$app/Contents/Resources/AppIcon.icns"
cp "$icons"/macos-menubar/menubar-{off,on,live}{,@2x}.png "$app/Contents/Resources/"

# Load frameworks from the bundle, and keep no paths of the build machine:
# release builds still carry a debug map (OSO entries naming each object
# file under this checkout), so strip it along with local symbols.
exe="$app/Contents/MacOS/AVRelay"
strip -S -x "$exe"
install_name_tool -add_rpath @executable_path/../Frameworks "$exe"
otool -l "$exe" | awk '/cmd LC_RPATH/ { getline; getline; print $2 }' | sort -u | while read -r path; do
	case $path in
	@* | /usr/lib/*) ;;
	*) install_name_tool -delete_rpath "$path" "$exe" ;;
	esac
done

# Sign inside out: ad hoc by default (no Apple Developer ID), or with the given identity.
identity=${AVRELAY_SIGN_IDENTITY:--}
sign=(codesign --force --timestamp=none --sign "$identity")
if [[ -n ${AVRELAY_SIGN_KEYCHAIN:-} ]]; then
	if [[ -n ${AVRELAY_SIGN_KEYCHAIN_PASSWORD_FILE:-} ]]; then
		# Through security's command mode on stdin: a -p argument would show in ps.
		printf 'unlock-keychain -p "%s" "%s"\n' "$(cat "$AVRELAY_SIGN_KEYCHAIN_PASSWORD_FILE")" "$AVRELAY_SIGN_KEYCHAIN" | security -i >/dev/null
	fi
	sign+=(--keychain "$AVRELAY_SIGN_KEYCHAIN")
fi
"${sign[@]}" "$app/Contents/Frameworks/WebRTC.framework"
"${sign[@]}" --identifier io.github.salihmarangoz.avrelay "$app"
codesign --verify --strict --deep "$app"
echo "Signed with: $( [[ $identity == - ]] && echo "ad hoc" || echo "$identity")"

name="AV-Relay-$version-macOS"
rm -f "$out/$name.zip" "$out/$name.dmg"
ditto -c -k --keepParent "$app" "$out/$name.zip"
staging=$(mktemp -d)
ditto "$app" "$staging/AV Relay.app"
ln -s /Applications "$staging/Applications"
hdiutil create -quiet -volname "AV Relay" -srcfolder "$staging" -format UDZO -ov "$out/$name.dmg"
rm -rf "$staging"
echo "Built $app, $out/$name.zip and $out/$name.dmg"
