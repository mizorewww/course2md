# Releases and package channels

| Channel | GUI | CLI |
| --- | --- | --- |
| Homebrew tap `mizorewww/tap` | Cask `course2md-gui` (Apple Silicon, macOS 15+) | Formula `course2md` |
| Homebrew prerelease channels | Cask `course2md-gui@{alpha,beta,rc}` | Formula `course2md-{alpha,beta,rc}` (keg-only) |
| AUR (stable only) | `course2md-gui-bin` (x86_64) | `course2md-bin` (x86_64, aarch64) |

GUI packages contain a matching private CLI engine and do not install a terminal `course2md` command, so they coexist with the CLI package. Both channels depend on ffmpeg, yt-dlp and aria2 (multi-connection model downloads); desktop archives also bundle the yt-dlp version pinned in `desktop/scripts/package.py`, and Windows archives bundle the official aria2 build pinned there.

## Publishing a version

1. Set the same SemVer version (`2.0.0`, or `2.0.0-rc.N` / `-beta.N` / `-alpha.N`) in `Cargo.toml` and `desktop/Cargo.toml`, update both lock files, and write `docs/releases/v<version>.md`, which becomes the release body.
2. Freeze the GPUI revisions you tested: `python3 desktop/scripts/sources.py --freeze`, then commit `desktop/sources.lock.json`.
3. Push tag `v<version>`, or run the `release` workflow with that version. It builds every CLI and desktop asset first, then publishes the GitHub Release with `SHA256SUMS`. Prerelease versions are marked as pre-releases and do not replace Latest.
4. `brew-tap` and `aur` run after a successful release. Homebrew updates only the release's channel; AUR skips prereleases and builds and checks the GUI package on Arch before pushing. Each pushes with its own key: `brew-tap` uses the `HOMEBREW_TAP_DEPLOY_KEY` secret (a deploy key with write access to `mizorewww/homebrew-tap` only) and `aur` uses `AUR_SSH_PRIVATE_KEY`.

macOS bundles keep the full SemVer in `Course2mdVersion` and use Apple's numeric form for bundle versions (RC2 → `2.0.0` / `2.0.0fc2`).

Recipes are rendered from `packaging/homebrew/` and `packaging/aur/`. Change these templates, not the external repositories, which later releases overwrite. To retry a channel, run `brew-tap` or `aur` manually with `version`; `aur` accepts `dry_run: true`, and `packaging/aur/update-aur.sh <version> --dry-run` runs the same validation locally on Arch.


User documentation lives in the [GitHub Wiki](https://github.com/mizorewww/course2md/wiki) (`git clone git@github.com:mizorewww/course2md.wiki.git`). Keep the Chinese and English pages aligned.
