# GQY · 顾清影

> 一个挂着 AI 助手外壳的桌面人格——终端、网页、QQ 里都住着同一个顾清影。
> A desktop AI assistant with a personality: the same GQY lives in your terminal, in your browser, and in QQ.

[![release](https://img.shields.io/github/v/release/Francis-Xavier-code/gqy)](https://github.com/Francis-Xavier-code/gqy/releases)
[![license](https://img.shields.io/github/license/Francis-Xavier-code/gqy)](LICENSE)

GQY（顾清影）是一个运行在 macOS 上的命令行 AI 助手：她是常驻的聊天人格，
也是你的终端工具人——装软件、查资料、跑命令、管知识库，都会干。

GQY is a macOS command-line AI assistant. She is a long-lived chat persona
and a terminal tool-bearer: installing software, looking things up, running
commands, and maintaining a knowledge base.

## 功能 Features

- 💬 **REPL 聊天** — 终端里的完整对话体验：软换行、历史浏览、粘贴折叠、
  图片转文字、长回复转图片，以及可选的键盘增强协议。
  Full REPL chat: soft wrapping, history browsing, paste folding,
  image-to-text, long-reply rendering, optional keyboard enhancement.
- 🌐 **Web 界面** — 内置 HTTP 服务，浏览器里同样可以聊天。
  Built-in web server so you can chat from a browser too.
- 🧰 **工具调用** — 知识库搜索、网页搜索、Todo 列表、后台任务、命令执行、
  Homebrew 安装查询、AppleGamingWiki 兼容性查询、man 手册查询等。
  Tool calling: knowledge base search, web search, todo lists, background
  jobs, command execution, Homebrew lookup, AppleGamingWiki compatibility,
  man pages, and more.
- 🧠 **长期记忆** — 记忆、日记、好感度、会话回放与压缩，聊得越久越了解你。
  Long-term memory, diary, affection, session replay and compaction.
- 📚 **知识库** — 可更新的默认知识库（kb/），问答先查证再作答。
  Updatable default knowledge base (kb/): verify before answering.
- ⚙️ **配置 TUI** — 交互式配置界面：模型、平台、QQ、快捷键一键设置。
  Interactive configuration TUI for models, platforms, QQ and hotkeys.

## 安装 Installation

### Homebrew

```sh
brew tap Francis-Xavier-code/gqy
brew install gqy
```

### 从 Release 下载

在 [Releases](https://github.com/Francis-Xavier-code/gqy/releases) 下载
`gqy-<version>-<arch>-apple-darwin.tar.gz`（`aarch64` 为 Apple Silicon，
`x86_64` 为 Intel），解压后把 `gqy` 放进 PATH：

```sh
tar -xzf gqy-0.1.0-aarch64-apple-darwin.tar.gz
sudo cp gqy-0.1.0-aarch64-apple-darwin/gqy /usr/local/bin/gqy
```

Download the matching `gqy-<version>-<arch>-apple-darwin.tar.gz` from
Releases, extract, and put `gqy` on your PATH.

### 从源码编译 From source

```sh
git clone https://github.com/Francis-Xavier-code/gqy.git
cd gqy
cargo build --release
# 产物在 target/release/gqy
```

需要 Rust 1.89+。Requires Rust 1.89+.

## 快速开始 Quick start

```sh
gqy            # 进入 REPL 聊天
gqy ask "hi"  # 一次性提问
gqy web        # 启动 Web 界面
gqy config     # 配置向导
gqy help       # 全部命令
```

首次启动会引导你配置模型（OpenAI 兼容 API / DeepSeek 等）。

First run walks you through model configuration (OpenAI-compatible API,
DeepSeek, etc.).

## 配置 Configuration

配置文件位于 `~/.config/gqy/config.jsonc`（可用 `gqy config` 图形化编辑）：

- `model` — 模型与提供商设置
- `platforms` — QQ / 终端 / Web 平台开关与权限
- `context` — 上下文窗口、压缩与缓存策略
- `plugins` — 工具与视觉插件

Config lives at `~/.config/gqy/config.jsonc` (edit it visually with
`gqy config`).

## 文档 Documentation

- [设计理念 Design philosophy](docs/理念.md) — 上下文与缓存设计（中文）
- [缓存与提示词计划 Cache & prompt plan](docs/cache-and-prompt-plan.md)（中文）
- [发布打包 Release packaging](packaging/README.md) — Homebrew formula 说明

## 开发 Development

```sh
cargo fmt --all --check   # 格式检查
cargo clippy --all-targets -- -D warnings
cargo test --all          # 测试
```

推送 `v*` 标签会自动触发完整流水线：测试 → 双架构构建 → GitHub Release
（二进制 + 源码 + Homebrew formula）。详见
[.github/workflows/release.yml](.github/workflows/release.yml)。

Pushing a `v*` tag runs the full pipeline: tests → dual-arch builds →
GitHub Release with binaries, source and the Homebrew formula.

## 许可 License

[MIT](LICENSE) © 2026 Francis-Xavier-code
