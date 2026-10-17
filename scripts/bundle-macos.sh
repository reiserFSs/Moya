#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"
cargo build --release -p moya-app --bin moya --target-dir "$root/target"
version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' "$root/Cargo.toml")
[ -n "$version" ] || { echo "Cannot read workspace version from Cargo.toml" >&2; exit 1; }
app=${1:-"$HOME/Applications/Moya.app"}
contents="$app/Contents"
mkdir -p "$contents/MacOS" "$contents/Resources"
install -m 755 "$root/target/release/moya" "$contents/MacOS/moya"
install -m 644 "$root/crates/moya-app/assets/Moya.icns" "$contents/Resources/Moya.icns"
cat > "$contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key>
    <string>local.moya.app</string>
    <key>CFBundleName</key>
    <string>Moya</string>
    <key>CFBundleExecutable</key>
    <string>moya</string>
    <key>CFBundleIconFile</key>
    <string>Moya.icns</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>$version</string>
    <key>CFBundleVersion</key>
    <string>$version</string>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
PLIST
# Seal the bundle (the linker signature only covers the bare executable).
codesign --force --sign - "$app"
printf 'Installed %s\n' "$app"
