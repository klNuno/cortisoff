#!/bin/sh
# Builds Cortisoff.app around the release executables, signs it ad hoc and
# zips it the way Finder expects (`ditto`, so the signature survives).
#
#   packaging/macos/bundle.sh <dir with cortisoff and cortisoff-cli> <version> <zip>
#
# Runs on macOS only: sips, iconutil, codesign and ditto ship with it. The
# installer writes the same Info.plist from the same template
# (crates/cortisoff-update/src/install/bundle.rs), so a bundle it rewrites
# after an update matches the one released.
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

codesign --force --deep --sign - "$app"
codesign --verify --deep --strict --verbose=2 "$app"

mkdir -p "$(dirname "$out")"
rm -f "$out"
ditto -c -k --sequesterRsrc --keepParent "$app" "$out"
ls -la "$out"
