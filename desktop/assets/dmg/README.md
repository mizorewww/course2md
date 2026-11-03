# course2md 安装窗口

![Finder 预览](preview.png)

`desktop/scripts/dmg.py` 用 [dmgbuild](https://dmgbuild.readthedocs.io/en/latest/) 直接写入 Finder 布局，不需要图形会话，打包后挂载成品检查布局与签名。`background.tiff` 同时包含 1× 和 2× 图像，`background@2x.png` 仅用于预览。

重新渲染背景并打包（需要 macOS）：

```sh
swift desktop/assets/dmg/render.swift
python3 desktop/scripts/package.py --debug --no-build
```
