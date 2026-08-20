# GQY 命令行参考

> gqy 是一个单二进制程序(`gqy`),同一入口服务三面:终端 REPL、内置 Web UI、
> 聊天平台(QQ/OneBot)。大部分状态在后台 daemon 中维护,CLI 是薄客户端:
> 交互命令(REPL、`ask`、`history` 等)走 IPC 交给 daemon 执行。

全局选项(适用于所有命令):

| 选项 | 说明 |
| --- | --- |
| `--debug` | 将详细诊断信息写入 GQY 日志目录 |
| `--stdout` | 纯文本输出模式(无颜色、无 TUI),适合管道重定向 |
| `--session <SESSION>` | 仅本次命令指定目标会话(名称或编号),不改变全局当前会话 |
| `-c, --continue` | 把消息发进终端集成会话,而不是用完即弃的一次性对话 |
| `-h, --help` | 显示帮助 |
| `-V, --version` | 显示版本 |

不带任何参数运行 `gqy` 进入 REPL;直接输入消息会发送一次对话。

---

## 会话与模式

| 命令 | 说明 |
| --- | --- |
| `gqy` | 无参数进入 REPL;直接跟消息则一次性对话 |
| `gqy normal` | 进入普通模式 REPL(人格全能力) |
| `gqy dev` | 进入开发模式 REPL(极简编码形态,无人格,独立工具集与独立记忆命名空间) |
| `gqy ask <消息>...` | 向助手发送一条消息,一次性对话 |
| `gqy tool-call [NAME] [ARGUMENTS]` | 工具桥:通过命令行调用 AI 工具 |

`tool-call` 选项:

| 选项 | 说明 |
| --- | --- |
| `--stdin` | 从标准输入读参数 JSON(跨 shell 安全,避免引号地狱) |
| `--args-file <PATH>` | 从文件读参数 JSON |
| `--list` | 列出当前可用工具(名称+显示名) |
| `--describe` | 打印指定工具的完整合同(描述+参数 schema) |

## 后台服务 daemon

| 子命令 | 说明 |
| --- | --- |
| `gqy daemon start` | 启动所有已配置的 GQY 接口 |
| `gqy daemon stop` | 停止 GQY 后台服务 |
| `gqy daemon restart` | 重启 GQY 后台服务 |
| `gqy daemon status` | 显示 daemon 与接口状态(WebUI 地址、QQ 连接等) |
| `gqy daemon logs [TOPIC]` | 持续查看 daemon 日志;`-n <N>` 仅输出最近 N 行后退出 |

daemon 全局选项:`--port <PORT>` 指定 WebUI TCP 端口。

## Web 界面

| 命令 | 说明 |
| --- | --- |
| `gqy web` | 访问本地 GQY WebUI |

选项:`--port <PORT>`(默认 8300)、`--bind <ADDR>`(默认 127.0.0.1;`0.0.0.0` 暴露到局域网,建议配合 `-p` 设置密码)、
`-p, --password`(安全输入访问密码)、`--password-file <PATH>`(从文件读密码)。

## 配置

| 命令 | 说明 |
| --- | --- |
| `gqy init` | 创建默认配置和状态文件 |
| `gqy config` | 使用 TUI 进行配置 |
| `gqy config validate` | 校验当前配置 |
| `gqy config paths` | 显示配置路径 |
| `gqy reload` | 重新加载配置 |
| `gqy paths` | 显示应用配置、数据和缓存路径 |
| `gqy export [OUTPUT]` | 导出配置为可移植归档 |
| `gqy import <ARCHIVE>` | 导入配置 |
| `gqy list-models` | 列出可用模型 |

`export` 选项:

| 选项 | 说明 |
| --- | --- |
| `--all` | 包含全部可移植数据(向量索引与平台历史) |
| `--index` | 包含知识库向量索引(很大;可用 `gqy kb embed` 重建) |
| `--platforms` | 包含通讯平台的聊天历史 |
| `--no-secrets` | 清空 API key 与访问令牌(导入后需自行补填) |
| `--dry-run` | 只打印将要打包的内容,不实际写归档 |
| `--force` | 覆盖已存在的归档文件 |

`import` 选项:`--force` 覆盖已有数据(覆盖前先备份当前安装)。

## 知识库 kb

| 子命令 | 说明 |
| --- | --- |
| `gqy kb add <PATH>` | 添加文件或目录(`-r/--recursive` 为兼容参数;目录默认递归导入) |
| `gqy kb list` | 列出已索引文件 |
| `gqy kb search [QUERY]...` | 搜索知识库内容(`-l/--limit` 最大结果数) |
| `gqy kb find [QUERY]...` | 按文件名查找文件(`-l/--limit`) |
| `gqy kb read <FILE>` | 读取知识库文件(`--start` 起始行,`--lines` 行数) |
| `gqy kb remove <FILE>` | 移除知识库文件 |
| `gqy kb reindex` | 按需重建关键词索引 |
| `gqy kb stats` | 显示知识库统计 |
| `gqy kb embed` | 管理语义嵌入(子命令 `reindex` 重建向量索引) |

## 记忆 memory

| 子命令 | 说明 |
| --- | --- |
| `gqy memory stats` | 显示记忆统计 |
| `gqy memory reset` | 清空助手记忆(`--include-skills` 同时移除自动生成的 skills) |
| `gqy memory search [QUERY]...` | 搜索记忆(`-l/--limit`,`--forgotten` 包含已遗忘记忆) |
| `gqy memory remember [CONTENT]...` | 手动保存事实(`-s/--source` 来源标签,默认 manual) |

## Skills

| 子命令 | 说明 |
| --- | --- |
| `gqy skills list` | 列出 skills |
| `gqy skills show <NAME>` | 显示 skill |
| `gqy skills enable <NAME>` | 启用 skill |
| `gqy skills disable <NAME>` | 禁用 skill |
| `gqy skills remove <NAME>` | 移除 skill |
| `gqy skills stats` | 显示 skill 统计 |
| `gqy skills prune` | 清理已禁用的自动 skills |
| `gqy skills import <PATH>` | 从本地路径导入 skill 目录（先审查后安装；`--force` 跳过审查直接安装，危险） |

> **安全模型**：`review_skill` / `review_script` 把脚本与 SKILL.md 内容交给 AI 审计，记录
> `allow` / `caution` / `block` 结论；`publish_skill` / `register_script` 只有在存在非 block
> 审查结论且用户明确确认后才放行。审查与安装必须分属不同轮次（guard 强制）。

## 资源审查 resources

| 子命令 | 说明 |
| --- | --- |
| `gqy resources status` | 查看脚本/Skill 的审查与安装状态 |
| `gqy resources prune` | 清理过期的审查与安装记录（审查 7 天过期） |

## 终端无缝集成

这些命令把 gqy 接进你的 shell,集成后可在终端直接用自然语言交流
(输入不存在的命令时,gqy 拦截并解释意图)。

| 命令 | 说明 |
| --- | --- |
| `gqy fish-init` | 集成到 fish |
| `gqy bash-init` | 集成到 bash |
| `gqy zsh-init` | 集成到 zsh |
| `gqy remove-shell-hook` | 安全删除已安装的 GQY shell hook |
| `gqy models [TARGET]` | 修改终端集成会话的模型(序号/供应商/模型名;`default` 恢复跟随全局池) |
| `gqy variant [NAME]` | 切换终端集成会话模型的思考档位(省略进入交互选择) |
| `gqy history` | 显示会话历史(`-l/--limit` 条数,默认 20;`--raw` 原始 JSONL;`--no-thinking` 隐藏思考内容) |
| `gqy reset` | 清除终端集成会话上下文 |
| `gqy reset-memory` | 清空长期记忆 |
| `gqy pop [COUNT]` | 将最旧对话轮次移出当前上下文(省略进入交互多选) |

## 维护

| 命令 | 说明 |
| --- | --- |
| `gqy update-default-kb` | 更新 GQY 默认知识库 |
| `gqy wipe` | 抹掉所有会话历史、记忆、群聊上下文及其产物(`--yes` 跳过确认,供非交互场景) |

---

## 环境变量

| 变量 | 说明 |
| --- | --- |
| `GQY_HOME` | 数据根目录(默认 `~/.gqy`) |
| `GQY_DIRECT` | `1` 时以直接模式运行 REPL,绕过 daemon(与 daemon 互斥) |
| `GQY_LANG` | `auto`/`zh`/`en`,驱动界面语言(覆盖配置 `display.language`) |
| `GQY_LOG_REQUESTS` | `1` 时记录出站 LLM 请求 |
| `GQY_SESSION` | 指定目标会话 |
| `GQY_TURN_ORIGIN` | 回合来源标记 |
| `GQY_MEMES_DIR` | 内置表情库目录(覆盖默认查找路径) |
| `RENDERER_FONTS_ENV` | 渲染字体目录(覆盖默认查找路径) |

更多背景见 [architecture.md](architecture.md)。
