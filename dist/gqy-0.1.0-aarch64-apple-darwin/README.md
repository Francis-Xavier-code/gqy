# 发布打包

- `brew/gqy.rb` — Homebrew formula。从 GitHub Release 下载 macOS 预编译
  资产,补上 Noto CJK 渲染字体后安装。发布前需要:

  1. 产出 macOS 资产并上传 Release,命名
     `gqy-<version>-aarch64-apple-darwin.tar.gz`(Intel 机器再加
     `x86_64-apple-darwin` 变体),并在 formula 里填上 `sha256`。
  2. 固定 `noto-sans-cjk-sc` 资源的版本与 `sha256`。
  3. 本地实测:`brew install --build-from-source ./gqy.rb`。

  用户侧安装(tap 发布后):

  ```
  brew tap Francis-Xavier-code/gqy
  brew install gqy
  ```

- `arch/` — 旧 AUR 发布打包,已随 Arch Linux 迁移移除。

## 资产与字体

| 路径 | 用途 | 来源 |
| --- | --- | --- |
| `bin/gqy` | 主程序 | Release 预编译资产 |
| `share/gqy/fonts/` | 长回复转图片的渲染字体 | Noto CJK 上游(formula 下载;发布资产不含字体) |
| 缺失字体时 | 长文转图静默退化为纯文本 | — |
