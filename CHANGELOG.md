# Changelog

本项目所有值得记录的改动都会列在此文件。格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [SemVer](https://semver.org/lang/zh-CN/)。

## [Unreleased]

### Fixed

- 在线安装脚本：目标目录不可写时提示使用 `sudo` 或 `PREFIX=~/.local`，不再裸报错并残留临时文件。

## [0.1.1] - 2026-08-21

### Fixed

- **ask_question 工具**：改为 `always_loaded`，LLM 始终可见 `questions` 数组 schema，避免模型把
  `questions` 序列化成字符串导致 `QuestionRequest::parse` 类型校验失败（PR #22，issue #21）。
- **ask_question 参数解析**：失败时错误信息附上可直接照抄的 JSON 数组示例（含整个参数被双重编码的情况），
  减少模型重试轮次。

### Added

- **在线安装脚本** `scripts/install.sh`：自动检测最新版本、适配架构（aarch64/x86_64），支持
  `PREFIX` 自定义目录与 `CNB_TOKEN` sha256 校验，随每次 Release 一并发布。
- **多平台仓库同步脚本** `scripts/sync-remotes.sh`：一键 fetch/pull/push CNB、GitHub、Gitee 三平台。
- 展示与详细介绍文档、项目图片资源。

### Changed

- 版本号升至 0.1.1（含 `Cargo.lock` 同步）。

## [0.1.0] - 2026-08-21

首个发布版本：单二进制 macOS CLI AI 助手（顾清影人格）。

### Added

- **三端一体**：同一个二进制承载终端 REPL、内置 WebUI、QQ（OneBot v11）平台传输。
- **daemon 架构**：后台常驻服务（Unix socket IPC，`PROTOCOL_VERSION` 守护），状态变更命令在
  daemon 与进程内执行间自动切换；`GQY_DIRECT=1` 提供无 daemon 的直接模式。
- **缓存契约（v7）**：stub 工具按需加载（`load_tools`）、字节稳定前缀、瞬态块化石回放、单调压缩，
  长会话提示词前缀命中率优先。
- **在线人设**：顾清影人格提示词构建期混淆打包，终端/WebUI/QQ 三端一致。
- **多架构发布**：macOS aarch64/x86_64 + Linux musl 静态二进制（zig 交叉编译），Homebrew formula 与
  在线安装脚本配套。
- 结构拆分：超大模块按 ~1500 行拆分，命名规范化，架构文档同步。

### Fixed

- 多平台仓库历史分歧的合并基准确认（CNB 为 canonical main）。
- Homebrew formula 回写改推送到默认分支（避免推送到已存在的 tag 被拒）。
