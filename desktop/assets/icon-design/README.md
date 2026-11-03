# course2md 应用图标

`Course2MD.icon` 是 Icon Composer 的可编辑源文件；`desktop/assets/` 下的 `icon.icns`、`icon.ico`、`icon.png` 与小尺寸 PNG 都由它导出，不要直接改。

在 Icon Composer 中修改并保存后，在安装了 Xcode（含 Icon Composer）与 Pillow 的 macOS 上运行：

```sh
python3 desktop/assets/icon-design/export.py
```

原生渲染图和预览板写入 `exports/`（不纳入版本库）。`build.rs` 把 `icon.ico` 编译为 Windows 的编号 `1` 资源，修改后要重新构建 Windows 程序；打包时会检查 `.exe` 嵌入了它的每个尺寸。
