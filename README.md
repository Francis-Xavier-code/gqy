# GQY · 顾清影

> 一个挂着 AI 助手外壳的桌面人格——终端、网页、QQ 里都住着同一个顾清影。
> A desktop AI assistant with a personality: the same GQY lives in your terminal, your browser, and in QQ.

<p align="center">
  <a href="https://cnb.cool/xynrin.ptt/GQY/-/releases">
    <img alt="release" src="https://cnb.cool/xynrin.ptt/GQY/-/badge/release" />
  </a>
  <a href="https://cnb.cool/xynrin.ptt/GQY/-/pipelines">
    <img alt="CI" src="https://cnb.cool/xynrin.ptt/GQY/-/badge/git/latest/ci/status/push?branch=main" />
  </a>
  <a href="https://cnb.cool/xynrin.ptt/GQY/-/blob/main/LICENSE">
    <img alt="license" src="https://img.shields.io/badge/license-MIT-8A2BE2" />
  </a>
  <img alt="platform" src="https://img.shields.io/badge/platform-macOS-333333?logo=apple&logoColor=white" />
  <img alt="rust" src="https://img.shields.io/badge/Rust-1.89+-orange?logo=rust&logoColor=white" />
  <img alt="language" src="https://img.shields.io/badge/language-中文/English-8A2BE2" />
</p>

GQY（顾清影）是一个运行在 macOS 上的命令行 AI 助手：她是常驻的聊天人格，
也是你的终端工具人——装软件、查资料、跑命令、管知识库，都会干。

GQY is a macOS command-line AI assistant. She is a long-lived chat persona
and a terminal tool-bearer: installing software, looking things up, running
commands, and maintaining a knowledge base.

---

## 目录 Contents

- [功能 Features](#功能-features)
- [安装 Installation](#安装-installation)
- [快速开始 Quick start](#快速开始-quick-start)
- [配置 Configuration](#配置-configuration)
- [Skill 与脚本安全 Skill & script safety](#skill-与脚本安全-skill--script-safety)
- [文档 Documentation](#文档-documentation)
- [开发 Development](#开发-development)
- [许可 License](#许可-license)

---

## 功能 Features

| | 功能 | 说明 |
|---|---|---|
| 💬 | **REPL 聊天** | 软换行、历史浏览、粘贴折叠、图片转文字、长回复转图片、可选的键盘增强协议。<br>*Soft wrapping, history, paste folding, image-to-text, long-reply rendering, keyboard enhancement.* |
| 🌐 | **Web 界面** | 内置 HTTP 服务，浏览器里同样可以聊天；「设置 → 平台」可管理第三方通信平台。<br>*Built-in web server so you can chat from a browser too, with a Platforms panel for chat platforms.* |
| 🧰 | **工具调用** | 知识库、网页搜索、Todo、后台任务、命令执行、Homebrew 查询、AppleGamingWiki、man 手册等。<br>*KB search, web search, todos, background jobs, shell, Homebrew, AppleGamingWiki, man pages.* |
| 🧠 | **长期记忆** | 记忆、日记、好感度、会话回放与压缩，聊得越久越了解你。<br>*Memory, diary, affection, session replay & compaction.* |
| 📚 | **知识库** | 可更新的默认知识库（`kb/`），问答先查证再作答。<br>*Updatable default KB: verify before answering.* |
| 🔐 | **Skill/脚本安全门** | 新 Skill 与脚本先由 AI 审查（`review_skill`/`review_script`），用户确认后才安装，审查与安装分轮强制。<br>*Skills & scripts are AI-reviewed, user-confirmed, then installed.* |
| ⚙️ | **配置 TUI** | 模型、平台、QQ、快捷键一键设置。<br>*Interactive config TUI for models, platforms, QQ & hotkeys.* |

---

## 安装 Installation

### 🍺 Homebrew（推荐 Recommended）

```sh
brew tap Francis-Xavier-code/gqy
brew install gqy
```

> 二进制资产托管在 CNB Release；tap 仓库保留在 GitHub（Homebrew 生态惯例）。
> Binaries are hosted on CNB Releases; the tap stays on GitHub (Homebrew convention).

### 📦 从 Release 下载 From release

在 [CNB Releases](https://cnb.cool/xynrin.ptt/GQY/-/releases) 下载
`gqy-<version>-<arch>-apple-darwin.tar.gz`（`aarch64` 为 Apple Silicon，
`x86_64` 为 Intel），解压后放进 PATH：

```sh
tar -xzf gqy-0.1.0-aarch64-apple-darwin.tar.gz
sudo cp gqy-0.1.0-aarch64-apple-darwin/gqy /usr/local/bin/gqy
```

Download the matching archive from Releases, extract, and put `gqy` on your
PATH.

### 🔨 从源码编译 From source

```sh
git clone https://cnb.cool/xynrin.ptt/GQY.git
cd GQY
cargo build --release
# 产物在 target/release/gqy
```

镜像仓库也可直接 clone（任一即可）：
`https://github.com/Francis-Xavier-code/gqy.git` 或
`https://gitee.com/Xynrin/GQY.git`。

需要 Rust 1.89+。Requires Rust 1.89+.

---

## 快速开始 Quick start

```sh
gqy            # 进入 REPL 聊天
gqy ask "hi"   # 一次性提问
gqy web        # 启动 Web 界面
gqy config     # 配置向导
gqy help       # 全部命令
```

首次启动会引导你配置模型（OpenAI 兼容 API / DeepSeek 等）。

First run walks you through model configuration (OpenAI-compatible API,
DeepSeek, etc.).

> **提示 Tip**：第三方通信平台（QQ/OneBot）由后台 daemon 统一托管，默认关闭。
> 需要时在配置（`platforms.transports` 或 WebUI「设置 → 平台」）中启用
> `qq` 并自行接入 NapCat（反向 WebSocket）；也可用
> `gqy platform enable qq` / `gqy platform status` 管理。
> Third-party chat platforms (QQ/OneBot) are hosted by the daemon and off by
> default. Enable `qq` in config (or WebUI Settings → Platforms) and connect
> NapCat yourself; manage via `gqy platform ...`.

---

## 配置 Configuration

配置文件位于 `~/.gqy/config/config.jsonc`（可用 `gqy config` 图形化编辑；
根目录可用 `GQY_HOME` 环境变量覆盖）：

| 配置段 | 说明 |
| --- | --- |
| `model` | 模型与提供商设置 Models & providers |
| `platforms` | 平台开关与权限；`transports` 段为第三方通信平台（daemon 托管）Platform switches & permissions; `transports` for daemon-hosted chat platforms |
| `context` | 上下文窗口、压缩与缓存策略 Context, compaction & caching |
| `plugins` | 工具与视觉插件 Tools & vision plugins |

Config lives at `~/.gqy/config/config.jsonc` (edit it visually with
`gqy config`; override the root with `GQY_HOME`).

---

## Skill 与脚本安全 Skill & script safety

GQY 的 Skill 与用户脚本采用「**AI 审查 → 用户确认 → 安装**」三步安全门：

1. **审查**：新脚本/技能放进 `scripts/` 或作为 skill 草稿后，AI 会先调用
   `review_script` / `review_skill` 阅读全部内容，给出 `allow`（安全）/
   `caution`（中风险）/ `block`（危险）结论并记录原因。
2. **确认**：只有用户看过审查并明确同意后，`register_script` /
   `publish_skill` 才允许执行；审查与安装被强制分成不同轮次，不能一轮内
   又审又装。
3. **可追溯**：`gqy resources status` 查看所有审查与安装记录；
   `gqy skills import <path>` 从本地导入 skill 目录走同一安全门。

详见 [docs/cli.md](docs/cli.md) 与 [docs/architecture.md](docs/architecture.md)。

---

## 文档 Documentation

- [架构概览 Architecture](docs/architecture.md) — 架构图 + 模块职责（中英）
- [CLI 命令参考 CLI reference](docs/cli.md) — 全部命令、选项与环境变量
- [设计理念 Design philosophy](docs/理念.md) — 上下文与缓存设计（中文）
- [发布打包 Release packaging](packaging/README.md) — CNB Release + Homebrew formula

---

## 开发 Development

```sh
cargo fmt --all --check                  # 格式检查 Format
cargo clippy --all-targets -- -D warnings  # Lint
cargo test --all                         # 全量测试(~1400)
```

推送 `v*` 标签会自动触发 CNB 流水线（`.cnb.yml`）：测试 → 双架构 macOS
交叉编译 + 双架构 Linux 静态交叉编译（Linux 容器内 zig 产出）→ 上传
[CNB Release](https://cnb.cool/xynrin.ptt/GQY/-/releases)
（二进制 + Homebrew formula 回写）。构建环境见
[.cnb/Dockerfile](.cnb/Dockerfile) 与 [packaging/README.md](packaging/README.md)。

Pushing a `v*` tag runs the CNB pipeline: tests → dual-arch macOS builds
+ dual-arch Linux static builds → CNB Release with binaries and a
regenerated Homebrew formula.

### 多平台镜像 Multi-platform mirrors

本仓库同时镜像在三个代码托管平台：CNB（`origin`）、GitHub、Gitee。

| 平台 | 地址 | remote |
| --- | --- | --- |
| CNB | `https://cnb.cool/xynrin.ptt/GQY.git` | `origin` |
| GitHub | `https://github.com/Francis-Xavier-code/gqy.git` | `github` |
| Gitee | `https://gitee.com/Xynrin/GQY.git` | `gitee` |

使用 [`scripts/sync-remotes.sh`](scripts/sync-remotes.sh) 一键拉取/推送三个远端：

```sh
bash scripts/sync-remotes.sh           # fetch 全部并显示三平台分歧摘要
bash scripts/sync-remotes.sh pull      # 以 CNB 为准拉取合并到本地
bash scripts/sync-remotes.sh push      # 把本地 main 推送到三个远端
bash scripts/sync-remotes.sh tags      # 对比三平台 tag 差异
```

> ⚠️ 三个平台的 main 历史曾**各自分歧**（截至最近一次同步，Gitee 领先约
> 400+ 提交、GitHub 停留在旧版本）。**跨平台合并前**请先跑
> `bash scripts/sync-remotes.sh` 查看各平台领先/落后数，确认以哪个平台为
> 准，再手动做合并——切勿盲目 `push` 覆盖。CI 发布流水线（`.cnb.yml`）
> 当前仅在 CNB 上运行。

---

## 许可 License

[MIT](LICENSE) © 2026 [Black Cat](https://cnb.cool/u/cnb.c4eDIjcWgOA)
