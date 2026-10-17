#!/bin/sh
set -eu

if [ "$(uname -s)" != Darwin ] || [ "$(uname -m)" != arm64 ]; then
    echo "Build this release on an Apple Silicon Mac (without Rosetta)." >&2
    exit 1
fi
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' "$root/Cargo.toml")
[ -n "$version" ] || { echo "Cannot read workspace version from Cargo.toml" >&2; exit 1; }
name="Moya-$version-macos-arm64"
dist="$root/target/release-dist"
stage="$dist/$name"
mkdir -p "$dist"
# Recreate only this script's staging directory, never an installed app.
rm -rf "$stage"
mkdir -p "$stage"
sh "$root/scripts/bundle-macos.sh" "$stage/Moya.app"
codesign --force --deep --sign - "$stage/Moya.app"
codesign -v "$stage/Moya.app"

# Ship runtime files only; development checks and packaging files stay out.
plugin=$(mktemp -d "$dist/plugin.XXXXXX")
trap 'rm -rf "$plugin"' EXIT HUP INT TERM
cp "$root/ps-plugin/manifest.json" "$root/ps-plugin/index.html" "$root/ps-plugin/index.js" "$plugin/"
cp -R "$root/ps-plugin/icons" "$plugin/icons"
(cd "$plugin" && /usr/bin/zip -q -r "$stage/Moya Photoshop Plugin.ccx" .)

cat > "$stage/Install Moya.command" <<'INSTALL'
#!/bin/sh
set -eu

dry_run=false
case "${1:-}" in
    --dry-run) dry_run=true ;;
    '') ;;
    *) echo "Usage: $0 [--dry-run]" >&2; exit 2 ;;
esac
if [ "$#" -gt 1 ]; then
    echo "Usage: $0 [--dry-run]" >&2
    exit 2
fi
root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
app="$root/Moya.app"
plugin="$root/Moya Photoshop Plugin.ccx"
upi='/Library/Application Support/Adobe/Adobe Desktop Common/RemoteComponents/UPI/UnifiedPluginInstallerAgent/UnifiedPluginInstallerAgent.app/Contents/MacOS/UnifiedPluginInstallerAgent'
if [ ! -d "$app" ] || [ ! -f "$plugin" ]; then
    echo 'Keep this installer beside Moya.app and Moya Photoshop Plugin.ccx.' >&2
    exit 1
fi
destination=/Applications
if [ ! -w "$destination" ] || { [ -e "$destination/Moya.app" ] && [ ! -w "$destination/Moya.app" ]; }; then
    destination="$HOME/Applications"
fi
if [ -L "$destination/Moya.app" ]; then
    echo "Refusing to overwrite an application symlink: $destination/Moya.app" >&2
    exit 1
fi
if "$dry_run"; then
    printf '[dry-run] Create directory: %s\n' "$destination"
    printf '[dry-run] Copy: %s -> %s/Moya.app\n' "$app" "$destination"
    printf '[dry-run] Clear quarantine on installed app only: %s/Moya.app\n' "$destination"
else
    mkdir -p "$destination"
    ditto "$app" "$destination/Moya.app"
    # Never clear quarantine recursively on the download folder or the plugin.
    xattr -dr com.apple.quarantine "$destination/Moya.app"
    printf 'Installed app: %s/Moya.app\n' "$destination"
fi

if pgrep -f '/Adobe Photoshop[^/]*\.app/Contents/MacOS/Adobe Photoshop' >/dev/null; then
    echo 'Photoshop is running: the plugin installation was skipped.'
    echo 'Quit Photoshop and rerun Install Moya.command to install the plugin.'
    echo 'The app installation is independent of the plugin installation.'
    exit 0
fi
if [ -x "$upi" ]; then
    if "$dry_run"; then
        printf '[dry-run] Install plugin: %s --install %s\n' "$upi" "$plugin"
    else
        "$upi" --install "$plugin"
    fi
else
    if "$dry_run"; then
        printf '[dry-run] Open plugin with Creative Cloud: open %s\n' "$plugin"
    else
        open "$plugin"
    fi
    echo 'Finish the plugin installation in Creative Cloud and approve any prompts.'
fi
echo 'Start Photoshop, then choose Plugins > Moya.'
echo 'If macOS blocks Moya.app, right-click it in Finder and choose Open.'
INSTALL
chmod 755 "$stage/Install Moya.command"
cat > "$stage/README.txt" <<README
Moya $version — macOS Apple Silicon

Requires an Apple Silicon Mac with Metal support, Photoshop 24 or later,
and Adobe Creative Cloud for installing the Photoshop plugin.

1. Unzip $name.zip.
2. Quit Photoshop, then double-click Install Moya.command.
   If macOS blocks the installer, right-click it in Finder and choose Open.
   The app goes into /Applications, or ~/Applications if that is not writable.
   If Photoshop is running, only the app installs: quit Photoshop and rerun.
3. Approve the Creative Cloud plugin installation if prompted.
4. Start Photoshop and choose Plugins > Moya.

Moya is ad-hoc signed, not Apple-notarized. If macOS blocks Moya.app on first
launch, right-click it in Finder and choose Open. Do not disable Gatekeeper.

Manual alternative: drag Moya.app into /Applications (or ~/Applications),
then double-click Moya Photoshop Plugin.ccx and approve its installation.
Quit Photoshop before installing the plugin, then start it again afterward.

To preview installer actions without changing anything, run in Terminal:
  ./Install\ Moya.command --dry-run
README

archive="$dist/$name.zip"
rm -f "$archive"
ditto -c -k --norsrc --noextattr --keepParent "$stage" "$archive"
shasum -a 256 "$archive"
printf 'Release archive: %s\n' "$archive"
