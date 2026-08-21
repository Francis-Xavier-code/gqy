# 发布打包

## 发布资产（CNB Release）

`v*` 标签触发 `.cnb.yml` 发布流水线，产出 **macOS 双架构 + Linux 双架构**
预编译资产并上传 CNB Release：

| 目标 | 架构 | 方式 |
| --- | --- | --- |
| macOS | aarch64-apple-darwin | zig 交叉编译（Linux 容器内） |
| macOS | x86_64-apple-darwin | zig 交叉编译（Linux 容器内） |
| Linux | aarch64-unknown-linux-musl | zig 交叉编译（静态 musl） |
| Linux | x86_64-unknown-linux-musl | zig 交叉编译（静态 musl） |

Linux 使用 musl 静态目标，产出单一静态可执行文件，可移植到任意
Linux 发行版；alsa（cpal 音频后端）静态链接，无需系统依赖。

## Homebrew（macOS）

- `brew/gqy.rb` — Homebrew formula。从 **CNB Release** 下载 macOS 预编译
  资产，补上 Noto CJK 渲染字体后安装。发布前需要:

  1. 产出 macOS 资产并上传 CNB Release，命名
     `gqy-<version>-aarch64-apple-darwin.tar.gz`（Intel 机器再加
     `x86_64-apple-darwin` 变体），并在 formula 里填上 `sha256`。
  2. 固定 `noto-sans-cjk-sc` 资源的版本与 `sha256`。
  3. 本地实测:`brew install --build-from-source ./gqy.rb`。

  用户侧安装(tap 发布后):

  ```
  brew tap Francis-Xavier-code/gqy
  brew install gqy
  ```

  资产 URL 形如:
  `https://cnb.cool/xynrin.ptt/GQY/-/releases/download/<tag>/<file>`。

## brew 与 CNB 的关系（当前结论）

- **发布资产托管在 CNB Release**（不再上传 GitHub Release）。CNB Release
  提供稳定的 `/releases/download/<tag>/<file>` 下载地址，适合 Homebrew
  formula 直接引用。
- **tap 仓库仍在 GitHub**（`Francis-Xavier-code/homebrew-gqy`）。Homebrew
  生态本身与 GitHub 强绑定（`brew tap <user>/<repo>` 默认从 GitHub 拉取，
  formula 的 `url` 字段可以是任意 HTTPS 地址）。因此：
  - 二进制分发走 CNB（本项目仓库的产物）；
  - 用户安装入口继续走 GitHub tap（社区惯例，改名成本高、收益低）。
- 若未来希望完全脱离 GitHub，可把 tap 仓库也迁移到 CNB，并将 formula
  放入 `homebrew-gqy` tap 的 CNB 仓库中；`brew tap` 仅支持 GitHub 路径，
  届时需用 `brew tap --custom-remote` 或直接下载 formula 安装。

## 资产与字体

| 路径 | 用途 | 来源 |
| --- | --- | --- |
| `bin/gqy` | 主程序 | CNB Release 预编译资产 |
| `share/gqy/fonts/` | 长回复转图片的渲染字体 | Noto CJK 上游(formula 下载;发布资产不含字体) |
| 缺失字体时 | 长文转图静默退化为纯文本 | — |

## 发布流水线（CNB）

`v*` 标签触发 `.cnb.yml`：测试 → 双架构 macOS 交叉编译 + 双架构 Linux
静态交叉编译（Linux 容器内 zig 产出，并行 job）→ 上传 CNB Release 资产
（`cnbcool/attachments` 插件）→ 回写 formula sha256。旧 GitHub Actions
流水线 `.github/workflows/release.yml` 已随迁移删除。
