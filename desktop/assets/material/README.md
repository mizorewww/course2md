# Embedded Material Icons

Original 24px **Material Icons Rounded** SVGs from Google's
[material-design-icons](https://github.com/google/material-design-icons), Apache-2.0.
`sources.json` records the upstream revision and original category/name of each icon.
SVG artwork is unmodified; filenames match GPUI Component's asset paths.

`desktop/src/icons.rs` embeds these files with `include_bytes!` and supplies the
application's `AssetSource`. This also replaces shared component icons (checkboxes,
input visibility controls, chevrons and window controls) without changing upstream
source. Unused component assets remain available through the upstream asset source.
No network request or installed icon font is required at runtime.

Navigation uses 20px icons. Compact controls retain their component sizing.
`task` and `refresh` have separate meanings; the loader asset is reserved for loading.
The icon license is included in all archives and inside the macOS application bundle.
