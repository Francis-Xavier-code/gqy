# GQY · 顾清影

> 一个挂着 AI 助手外壳的桌面人格——终端、网页、QQ 里都住着同一个顾清影。
> A desktop AI assistant with a personality: the same GQY lives in your terminal, your browser, and in QQ.

<p align="center">
  <a href="https://github.com/Francis-Xavier-code/gqy/releases">
    <img alt="release" src="https://img.shields.io/github/v/release/Francis-Xavier-code/gqy?color=blue&label=release" />
  </a>
  <a href="https://github.com/Francis-Xavier-code/gqy/actions">
    <img alt="tests" src="https://img.shields.io/github/actions/workflow/status/Francis-Xavier-code/gqy/release.yml?label=CI" />
  </a>
  <a href="LICENSE">
    <img alt="license" src="https://img.shields.io/github/license/Francis-Xavier-code/gqy" />
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
- [文档 Documentation](#文档-documentation)
- [开发 Development](#开发-development)
- [许可 License](#许可-license)

---

## 功能 Features

| | 功能 | 说明 |
|---|---|---|
| 💬 | **REPL 聊天** | 软换行、历史浏览、粘贴折叠、图片转文字、长回复转图片、可选的键盘增强协议。<br>*Soft wrapping, history, paste folding, image-to-text, long-reply rendering, keyboard enhancement.* |
| 🌐 | **Web 界面** | 内置 HTTP 服务，浏览器里同样可以聊天。<br>*Built-in web server so you can chat from a browser too.* |
| 🧰 | **工具调用** | 知识库、网页搜索、Todo、后台任务、命令执行、Homebrew 查询、AppleGamingWiki、man 手册等。<br>*KB search, web search, todos, background jobs, shell, Homebrew, AppleGamingWiki, man pages.* |
| 🧠 | **长期记忆** | 记忆、日记、好感度、会话回放与压缩，聊得越久越了解你。<br>*Memory, diary, affection, session replay & compaction.* |
| 📚 | **知识库** | 可更新的默认知识库（`kb/`），问答先查证再作答。<br>*Updatable default KB: verify before answering.* |
| ⚙️ | **配置 TUI** | 模型、平台、QQ、快捷键一键设置。<br>*Interactive config TUI for models, platforms, QQ & hotkeys.* |

---

## 安装 Installation

### 🍺 Homebrew（推荐 Recommended）

```sh
brew tap Francis-Xavier-code/gqy
brew install gqy
```

### 📦 从 Release 下载 From release

在 [Releases](https://github.com/Francis-Xavier-code/gqy/releases) 下载
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
git clone https://github.com/Francis-Xavier-code/gqy.git
cd gqy
cargo build --release
# 产物在 target/release/gqy
```

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

> **提示 Tip**：QQ 平台默认关闭。需要时在配置中开启
> `platforms.qq.enabled` 并自行接入 NapCat（反向 WebSocket）。
> QQ is off by default; enable it in config and connect NapCat yourself.

---

## 配置 Configuration

配置文件位于 `~/.config/gqy/config.jsonc`（可用 `gqy config` 图形化编辑）：

| 配置段 | 说明 |
| --- | --- |
| `model` | 模型与提供商设置 Models & providers |
| `platforms` | QQ / 终端 / Web 平台开关与权限 Platform switches & permissions |
| `context` | 上下文窗口、压缩与缓存策略 Context, compaction & caching |
| `plugins` | 工具与视觉插件 Tools & vision plugins |

Config lives at `~/.config/gqy/config.jsonc` (edit it visually with
`gqy config`).

---

## 文档 Documentation

- [设计理念 Design philosophy](docs/理念.md) — 上下文与缓存设计（中文）
- [缓存与提示词计划 Cache & prompt plan](docs/cache-and-prompt-plan.md)（中文）
- [发布打包 Release packaging](packaging/README.md) — Homebrew formula 说明

---

## 开发 Development

```sh
cargo fmt --all --check                  # 格式检查 Format
cargo clippy --all-targets -- -D warnings  # Lint
cargo test --all                         # 全量测试(~1400)
```

推送 `v*` 标签会自动触发完整流水线：测试 → 双架构构建 → GitHub Release
（二进制 + Homebrew formula 回写）。详见
[.github/workflows/release.yml](.github/workflows/release.yml)。

Pushing a `v*` tag runs the full pipeline: tests → dual-arch builds →
GitHub Release with binaries and a regenerated Homebrew formula.

---

## 许可 License

[MIT](LICENSE) © 2026 Francis-Xavier-code
