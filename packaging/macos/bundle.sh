#!/bin/sh
# Builds Cortisoff.app around the release executables, signs it and zips it
# the way Finder expects (`ditto`, so the signature survives).
#
#   packaging/macos/bundle.sh <dir with cortisoff and cortisoff-cli> <version> <zip>
#
# Runs on macOS only: sips, iconutil, codesign and ditto ship with it. The
# installer reads the same Info.plist template
# (crates/cortisoff-update/src/install/bundle.rs) and checks an update's
# bundle against it.
set -eu

bin=$1
version=$2
out=$3

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

app="$work/Cortisoff.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin/cortisoff" "$bin/cortisoff-cli" "$app/Contents/MacOS/"
chmod 755 "$app/Contents/MacOS/cortisoff" "$app/Contents/MacOS/cortisoff-cli"

sed "s/@VERSION@/$version/g" "$here/Info.plist" > "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist"

# Every size Finder and the Dock ask for, from the 512 px logo. The 1024 px
# slot (512@2x) stays empty rather than holding an upscaled copy.
iconset="$work/cortisoff.iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" "$root/assets/icon.png" --out "$iconset/icon_${size}x${size}.png" > /dev/null
  double=$((size * 2))
  if [ "$double" -le 512 ]; then
    sips -z "$double" "$double" "$root/assets/icon.png" --out "$iconset/icon_${size}x${size}@2x.png" > /dev/null
  fi
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/cortisoff.icns"

# Signed with the release identity when keychain.sh imported it, ad hoc
# otherwise. The requirement is spelled out: macOS files Accessibility,
# Input Monitoring and Screen Recording grants under it, so it names the
# bundle id and the certificate, never this build's hash, and the next
# release still matches it. The client is signed first, as its own code.
identity="${CORTISOFF_SIGN_IDENTITY:-}"
if [ -n "$identity" ]; then
  set -- --force --timestamp=none --keychain "$CORTISOFF_KEYCHAIN" --sign "$identity"
  codesign "$@" --identifier dev.mtsu.cortisoff.cli "$app/Contents/MacOS/cortisoff-cli"
  codesign "$@" -r="designated => identifier \"dev.mtsu.cortisoff\" and certificate leaf = H\"$identity\"" "$app"
else
  echo "No signing identity: signing ad hoc."
  codesign --force --sign - --identifier dev.mtsu.cortisoff.cli "$app/Contents/MacOS/cortisoff-cli"
  codesign --force --sign - "$app"
fi
codesign --verify --deep --strict --verbose=2 "$app"

requirement=$(codesign -d -r- "$app" 2>&1)
printf '%s\n' "$requirement"
if [ -n "$identity" ]; then
  printf '%s\n' "$requirement" | grep -q 'identifier "dev.mtsu.cortisoff"' ||
    { echo "::error::The designated requirement does not name dev.mtsu.cortisoff."; exit 1; }
  printf '%s\n' "$requirement" | grep -q "certificate leaf = H\"$identity\"" ||
    { echo "::error::The designated requirement does not name the signing certificate."; exit 1; }
  if printf '%s\n' "$requirement" | grep -q cdhash; then
    echo "::error::The designated requirement names a build hash."
    exit 1
  fi
fi

mkdir -p "$(dirname "$out")"
rm -f "$out"
ditto -c -k --sequesterRsrc --keepParent "$app" "$out"
ls -la "$out"
