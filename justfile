# Install this checkout as /Applications/course2md.app and open it.
install:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ "$(uname -s)" != Darwin ]]; then
        echo "just install is macOS-only." >&2
        exit 1
    fi
    # The scripts need tomllib (Python 3.11+); macOS's own python3 is older.
    python=""
    for candidate in python3.14 python3.13 python3.12 python3.11 python3; do
        if command -v "$candidate" >/dev/null &&
            "$candidate" -c 'import sys; sys.exit(sys.version_info < (3, 11))' 2>/dev/null; then
            python="$candidate"
            break
        fi
    done
    if [[ -z "$python" ]]; then
        echo "just install needs Python 3.11 or later (brew install python)." >&2
        exit 1
    fi
    # Prepare or refresh the GPUI sources at the locked revisions, replaying the
    # current reviewed patches; a remembered --developer-dir is reused.
    "$python" desktop/scripts/sources.py --locked
    app="$("$python" desktop/scripts/package.py --app-only | tail -1)"

    dest=/Applications/course2md.app
    if pgrep -f "$dest/Contents/MacOS/course2md-desktop" >/dev/null; then
        osascript -e 'tell application "course2md" to quit' >/dev/null 2>&1 || true
        # Give the running app a moment to save state before it is replaced.
        for _ in {1..30}; do
            pgrep -f "$dest/Contents/MacOS/course2md-desktop" >/dev/null || break
            sleep 0.3
        done
        if pgrep -f "$dest/Contents/MacOS/course2md-desktop" >/dev/null; then
            echo "course2md did not quit; refusing to replace the installed app." >&2
            exit 1
        fi
    fi
    staging=/Applications/.course2md.app.installing
    rm -rf "$staging"
    ditto "$app" "$staging"
    rm -rf "$dest"
    mv "$staging" "$dest"
    open "$dest"
