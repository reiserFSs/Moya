#!/bin/sh
set -eu

if [ "$(uname -s)" != Darwin ]; then
    echo "Installing the Moya Photoshop plugin requires macOS." >&2
    exit 1
fi
if pgrep -f '/Adobe Photoshop[^/]*\.app/Contents/MacOS/Adobe Photoshop' >/dev/null; then
    echo "Quit Photoshop before installing the Moya plugin. Installation requires restarting Photoshop." >&2
    exit 1
fi
if ! command -v python3 >/dev/null 2>&1; then
    echo "Install Python 3 before running the Moya plugin installer." >&2
    exit 1
fi

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
python3 - "$root/ps-plugin" <<'PY'
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

source = Path(sys.argv[1])
uxp = Path.home() / "Library/Application Support/Adobe/UXP"
registration = uxp / "PluginsInfo/v1/PS.json"
external = uxp / "Plugins/External"
plugin_id = "local.moya.bridge"
version = "1.0.0"
destination = external / f"{plugin_id}_{version}"

try:
    manifest = json.loads((source / "manifest.json").read_text())
    host = manifest["host"]
    hosts = host if isinstance(host, list) else [host]
    if (manifest["id"] != plugin_id or manifest["version"] != version
            or not any(h.get("app") == "PS" and h.get("minVersion") == "24.0.0"
                       for h in hosts)):
        raise ValueError("bridge manifest must specify local.moya.bridge 1.0.0 and Photoshop 24.0.0")
    if not isinstance(manifest.get("name"), str) or not manifest["name"].strip():
        raise ValueError("bridge manifest must include its display name")
    original = registration.read_bytes()
    data = json.loads(original)
    plugins = data["plugins"]
    if not isinstance(plugins, list) or not all(isinstance(p, dict) for p in plugins):
        raise ValueError("PS.json plugins must be an array of objects")
    entry = {
        "hostMinVersion": "24.0.0",
        "name": manifest["name"],
        "path": f"$localPlugins/External/{plugin_id}_{version}",
        "pluginId": plugin_id,
        "status": "enabled",
        "type": "uxp",
        "versionString": version,
    }
    updated = []
    inserted = False
    for plugin in plugins:
        if plugin.get("pluginId") == plugin_id:
            if not inserted:
                updated.append(entry)
                inserted = True
        else:
            updated.append(plugin)
    if not inserted:
        updated.append(entry)
    changed = updated != plugins
    data["plugins"] = updated
    external.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".moya-bridge-", dir=external) as work:
        staged = Path(work) / "plugin"
        previous = Path(work) / "previous"
        shutil.copytree(source, staged)
        # Recheck after staging so Photoshop cannot already be open at the cutover.
        running = subprocess.run(
            ["pgrep", "-f", r"/Adobe Photoshop[^/]*\.app/Contents/MacOS/Adobe Photoshop"],
            stdout=subprocess.DEVNULL, check=False,
        )
        if running.returncode != 1:
            raise RuntimeError("quit Photoshop before installing; restart it after installation")
        if registration.read_bytes() != original:
            raise RuntimeError("Photoshop registration changed during installation; nothing was installed")
        backup = None
        if changed:
            fd, backup = tempfile.mkstemp(prefix="PS.json.moya-bridge-backup-", dir=registration.parent)
            with os.fdopen(fd, "wb") as out:
                out.write(original)
            shutil.copystat(registration, backup)
        if destination.is_symlink():
            raise ValueError(f"refusing to replace symlink: {destination}")
        had_previous = destination.exists()
        if had_previous:
            destination.rename(previous)
        try:
            staged.rename(destination)
            if changed:
                fd, pending = tempfile.mkstemp(prefix=".PS.json.moya-bridge-", dir=registration.parent)
                try:
                    with os.fdopen(fd, "w") as out:
                        json.dump(data, out, separators=(",", ":"), ensure_ascii=False)
                    shutil.copystat(registration, pending)
                    os.replace(pending, registration)
                finally:
                    if os.path.exists(pending):
                        os.unlink(pending)
        except BaseException:
            if destination.exists():
                shutil.rmtree(destination)
            if had_previous:
                previous.rename(destination)
            raise
    print(f"Installed Moya plugin: {destination}")
    if backup:
        print(f"Registration backup: {backup}")
    print("Start Photoshop, then open Plugins > Moya.")
except (OSError, ValueError, KeyError, TypeError, RuntimeError) as error:
    print(f"Moya plugin installation failed: {error}", file=sys.stderr)
    sys.exit(1)
PY
