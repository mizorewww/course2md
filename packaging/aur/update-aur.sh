#!/usr/bin/env bash
# Render and validate both GUI and CLI packages, then push them to AUR.
# Usage: update-aur.sh <version> [--dry-run]
set -euo pipefail
VERSION="${1:?usage: update-aur.sh <version> [--dry-run]}"
python3 "$(dirname "$0")/../versioning.py" "$VERSION" || { echo "Invalid version: $VERSION" >&2; exit 1; }
MODE="${2:-}"
[[ -z "$MODE" || "$MODE" == '--dry-run' ]] || { echo 'Unknown option' >&2; exit 1; }
REPO="${COURSE2MD_REPO:-mizorewww/course2md}"
HERE="$(cd "$(dirname "$0")" && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
BASE="https://github.com/${REPO}/releases/download/v${VERSION}"
cd "$WORK"
curl --retry 3 -fsSLo linux-x86_64 "$BASE/course2md-linux-x86_64"
curl --retry 3 -fsSLo linux-aarch64 "$BASE/course2md-linux-aarch64"
curl --retry 3 -fsSLo gui.tar.gz "$BASE/course2md-desktop-linux-x86_64.tar.gz"
curl --retry 3 -fsSLo notices.txt "$BASE/course2md-third-party-licenses.txt"
curl --retry 3 -fsSLo LICENSE "https://raw.githubusercontent.com/${REPO}/v${VERSION}/LICENSE"
sha() { sha256sum "$1" | cut -d' ' -f1; }
mkdir cli gui
sed -e "s/@VERSION@/${VERSION}/g" \
    -e "s/@SHA_X86_64@/$(sha linux-x86_64)/" \
    -e "s/@SHA_AARCH64@/$(sha linux-aarch64)/" \
    -e "s/@SHA_LICENSE@/$(sha LICENSE)/" \
    -e "s/@SHA_NOTICES@/$(sha notices.txt)/" \
    "${HERE}/PKGBUILD.template" > cli/PKGBUILD
sed -e "s/@VERSION@/${VERSION}/g" \
    -e "s/@SHA_GUI@/$(sha gui.tar.gz)/" \
    "${HERE}/PKGBUILD-gui.template" > gui/PKGBUILD

# Build the actual archive on Arch before publishing any package recipe.
cp gui.tar.gz "gui/course2md-gui-bin-${VERSION}.tar.gz"
(
    cd gui
    makepkg --noconfirm
    desktop-file-validate pkg/course2md-gui-bin/usr/share/applications/course2md.desktop
    engine=./pkg/course2md-gui-bin/usr/lib/course2md-desktop/course2md
    "$engine" --version | tee version.txt
    grep -F "course2md ${VERSION} " version.txt
    for binary in course2md course2md-desktop; do
        ldd "pkg/course2md-gui-bin/usr/lib/course2md-desktop/$binary" > "$binary.ldd"
        cat "$binary.ldd"
        if grep -q 'not found' "$binary.ldd"; then exit 1; fi
    done
    test -L pkg/course2md-gui-bin/usr/bin/course2md-desktop
    test ! -e pkg/course2md-gui-bin/usr/bin/course2md
)
for kind in cli gui; do
    (cd "$kind"; makepkg --printsrcinfo > .SRCINFO)
done
if [[ "$MODE" == '--dry-run' ]]; then
    cat cli/.SRCINFO gui/.SRCINFO
    echo 'GUI build, dependencies, desktop entry and package metadata validated; nothing pushed.'
    exit 0
fi
for kind in cli gui; do
    if [[ "$kind" == cli ]]; then PKG=course2md-bin; else PKG=course2md-gui-bin; fi
    git clone -q "ssh://aur@aur.archlinux.org/${PKG}.git" "$kind-aur"
    cp "$kind/PKGBUILD" "$kind/.SRCINFO" "$kind-aur/"
    (
        cd "$kind-aur"
        git add PKGBUILD .SRCINFO
        if git diff --cached --quiet; then
            echo "AUR ${PKG} 已是 ${VERSION}，无需更新"
        else
            git commit -qm "${PKG} ${VERSION}-1"
            git push -q origin HEAD:master
            echo "AUR ${PKG} 已更新到 ${VERSION}"
        fi
    )
done
