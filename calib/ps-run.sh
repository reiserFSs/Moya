#!/bin/sh
set -eu
# ponytail: one Photoshop process requires a global lock; split only for separate PS instances.
lock="${TMPDIR:-/tmp}/moya-photoshop.lock"
while ! mkdir "$lock" 2>/dev/null; do sleep 1; done
trap 'rmdir "$lock"' EXIT HUP INT TERM
script=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
/usr/bin/osascript - "$script" <<'APPLESCRIPT'
on run argv
  with timeout of 86400 seconds
    tell application "Adobe Photoshop 2026"
      return do javascript file (item 1 of argv)
    end tell
  end timeout
end run
APPLESCRIPT
