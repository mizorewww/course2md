# Project instructions

## Desktop design

Before designing, implementing, or reviewing any desktop UI, read and follow
[course2md-design](.agents/skills/course2md-design/SKILL.md).
It is the current design authority, including when older mockups or design documents disagree.
The app is pure Material: colors, controls, icons and motion come from `gpui-kit-material`; do not create a new visual dialect in a page.

For a whole-product UI change, use subagents to independently review every affected page and its loading, empty, failure and completion states. Record concrete findings and their resolution. Verify the running native application, not just source or HTML mockups.

The user has excluded specialist accessibility audits. Preserve ordinary keyboard behavior and existing system preferences without expanding the task into an accessibility project.

## Conventions

Human-readable text is localized from [`i18n/catalog.tsv`](i18n/catalog.tsv) into English, Simplified Chinese, Traditional Chinese and Japanese. Engine and desktop call sites use `tr!` / `course2md::tr!`; desktop pages must not hard-code Chinese labels. The CLI follows the system locale; `--lang` and `COURSE2MD_LANG` override it. Machine-readable output (`--json`, `run-task`) stays in Simplified Chinese for stable consumers.

Before merging, run the checks in `.github/workflows/ci.yml` (engine) and `desktop.yml` (desktop). Releases follow [docs/PACKAGING.md](docs/PACKAGING.md).

Keep changes reviewable and make atomic Git commits by concern. Record history in commit messages, not in repository documents. Do not change release tags or replace published assets as part of an ordinary change.
