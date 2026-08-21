//! defs — 自 src/cli.rs 拆分。

pub(crate) use super::*;

#[derive(Debug, Parser)]
#[command(name = "gqy", version, about = "GQY CLI AI Agent")]
pub struct Cli {
    #[arg(long, global = true)]
    pub debug: bool,

    #[arg(long)]
    pub stdout: bool,

    /// 仅为本次命令指定目标会话（名称或编号），不改变全局当前会话
    #[arg(long)]
    pub session: Option<String>,

    #[arg(short = 'c', long = "continue", conflicts_with = "session")]
    pub continue_session: bool,

    #[arg(long, hide = true)]
    pub shell_intercept: bool,

    #[arg(long, hide = true)]
    pub shell_classify: bool,

    #[arg(long, hide = true)]
    pub shell: Option<String>,

    #[arg(long, hide = true)]
    pub stdin: bool,

    #[arg(long, hide = true)]
    pub clipboard_paste: bool,

    #[command(subcommand)]
    pub command: Option<Command>,

    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub message: Vec<String>,
}

pub fn parse() -> Cli {
    parse_args(std::env::args_os().collect()).unwrap_or_else(|err| err.exit())
}

pub(crate) fn parse_args(mut args: Vec<OsString>) -> std::result::Result<Cli, clap::Error> {
    let debug = extract_debug_flag(&mut args);
    let matches = localized_command().try_get_matches_from(args)?;
    let web_port_explicit = matches
        .subcommand_matches("web")
        .and_then(|web| web.value_source("port"))
        == Some(clap::parser::ValueSource::CommandLine);
    let mut cli = Cli::from_arg_matches(&matches)?;
    if let Some(Command::Web(args)) = &mut cli.command {
        args.port_explicit = web_port_explicit;
    }
    cli.debug |= debug;
    Ok(cli)
}

pub(crate) fn extract_debug_flag(args: &mut Vec<OsString>) -> bool {
    let mut debug = false;
    let mut index = 1;
    while index < args.len() {
        if args[index] == "--" {
            break;
        }
        if args[index] == "--debug" {
            args.remove(index);
            debug = true;
        } else {
            index += 1;
        }
    }
    debug
}

pub(crate) fn localized_command() -> clap::Command {
    let mut command = Cli::command();
    command = command
        .about(t("GQY AI assistant", "GQY AI 助手"))
        .override_usage(t(
            "gqy [OPTIONS] [MESSAGE]... [COMMAND]",
            "gqy [选项] [消息]... [命令]",
        ));
    if is_zh() {
        command = command
            .subcommand_help_heading("命令")
            .arg_required_else_help(false)
            .next_help_heading("选项")
            .help_template("{about}\n\n用法: {usage}\n\n命令:\n{subcommands}\n参数:\n{positionals}\n选项:\n{options}\n{after-help}")
            .after_help("提示：不带参数进入 REPL；直接输入消息会发送一次对话。可在配置界面设置语言，GQY_LANG 可临时覆盖。")
            .disable_help_subcommand(true);
    } else {
        command = command
            .after_help(
                "Tip: run without arguments to enter the REPL; pass MESSAGE to send one chat turn. Set the language in the configuration UI; GQY_LANG is a temporary override.",
            )
            .disable_help_subcommand(true);
    }
    command = localize_top_args(command);
    command = localize_subcommands(command);
    command = apply_localized_help_flags(command, true);
    if is_zh() {
        command = apply_chinese_help_template(command);
    }
    // 终端无缝集成组在根帮助里以静态段单独成节(这些子命令已 hide,
    // 不进 {subcommands});最后设置以免被上面的通用中文模板覆盖。
    command = command.help_template(root_help_template());
    command
}

pub(crate) fn root_help_template() -> String {
    let shell_block = t(
        "  fish-init          Integrate with fish; then chat in natural language directly in the terminal
  bash-init          Integrate with bash
  zsh-init           Integrate with zsh
  remove-shell-hook  Safely remove installed GQY shell hooks
  models             Switch the terminal-integration session's model
  variant            Switch the terminal session model's thinking level
  history            Show conversation history
  reset              Clear the terminal-integration session context
  reset-memory       Erase this persona's long-term memory
  pop                Move conversation turns out of active context",
        "  fish-init          集成到 fish，集成后可在终端直接使用自然语言交流
  bash-init          集成到 bash
  zsh-init           集成到 zsh
  remove-shell-hook  安全删除已安装的 GQY shell hook
  models             修改终端集成会话的模型
  variant            切换终端集成会话模型的思考档位
  history            显示会话历史
  reset              清除终端集成会话上下文
  reset-memory       清空长期记忆
  pop                将对话轮次移出当前上下文",
    );
    if is_zh() {
        format!(
            "{{about}}

用法: {{usage}}

命令:
{{subcommands}}

终端无缝集成相关：
{shell_block}

参数:
{{positionals}}
选项:
{{options}}
{{after-help}}"
        )
    } else {
        format!(
            "{{about}}

Usage: {{usage}}

Commands:
{{subcommands}}

Terminal integration:
{shell_block}

Arguments:
{{positionals}}
Options:
{{options}}
{{after-help}}"
        )
    }
}

pub(crate) fn apply_localized_help_flags(mut command: clap::Command, root: bool) -> clap::Command {
    command = command.disable_help_flag(true).arg(
        Arg::new("help")
            .short('h')
            .long("help")
            .help(t("Print help", "显示帮助"))
            .action(ArgAction::Help),
    );
    if root {
        command = command.disable_version_flag(true).arg(
            Arg::new("version")
                .short('V')
                .long("version")
                .help(t("Print version", "显示版本"))
                .action(ArgAction::Version),
        );
    }
    let subcommands = command
        .get_subcommands()
        .map(|subcommand| subcommand.get_name().to_string())
        .collect::<Vec<_>>();
    for name in subcommands {
        command = command.mut_subcommand(&name, |subcommand| {
            apply_localized_help_flags(subcommand, false)
        });
    }
    command
}

pub(crate) fn apply_chinese_help_template(mut command: clap::Command) -> clap::Command {
    let has_subcommands = command.get_subcommands().next().is_some();
    command = if has_subcommands {
        command.help_template(
            "{about}\n\n用法: {usage}\n\n命令:\n{subcommands}\n参数:\n{positionals}\n选项:\n{options}\n{after-help}",
        )
    } else {
        command.help_template(
            "{about}\n\n用法: {usage}\n\n参数:\n{positionals}\n选项:\n{options}\n{after-help}",
        )
    };
    let subcommands = command
        .get_subcommands()
        .map(|subcommand| subcommand.get_name().to_string())
        .collect::<Vec<_>>();
    for name in subcommands {
        command = command.mut_subcommand(&name, apply_chinese_help_template);
    }
    command
}

pub(crate) fn localize_top_args(command: clap::Command) -> clap::Command {
    command
        .mut_arg("debug", |arg| {
            arg.help(t(
                "Write detailed diagnostics to the GQY log directory",
                "将详细诊断信息写入 GQY 日志目录",
            ))
        })
        .mut_arg("stdout", |arg| {
            arg.help(t(
                "Plain output mode (no colors, no TUI); pipe-friendly for stdout redirection",
                "纯文本输出模式（无颜色、无 TUI）；适合管道重定向",
            ))
        })
        .mut_arg("continue_session", |arg| {
            arg.help(t(
                "Send the message into the terminal-integration session instead of a throwaway one-shot chat",
                "把消息发进终端集成会话，而不是用完即弃的一次性对话",
            ))
        })
        .mut_arg("message", |arg| {
            arg.help(t(
                "Message to send; omitted to enter REPL",
                "要发送的消息；省略则进入 REPL",
            ))
        })
}

pub(crate) fn localize_subcommands(mut command: clap::Command) -> clap::Command {
    let descriptions = [
        (
            "ask",
            "Send one message to the assistant as a one-shot chat",
            "向助手发送一条消息，一次性对话",
        ),
        (
            "normal",
            "Enter the normal-mode REPL (full persona abilities)",
            "进入普通模式 REPL（人格全能力）",
        ),
        (
            "dev",
            "Enter the dev-mode REPL (minimal coding form, no persona)",
            "进入开发模式 REPL（极简编码形态，无人格）",
        ),
        (
            "tool-call",
            "Tool bridge: call AI tools from the command line",
            "工具桥：通过命令行调用 AI 工具",
        ),
        (
            "init",
            "Create default config and state files",
            "创建默认配置和状态文件",
        ),
        (
            "paths",
            "Show app config, data, and cache paths",
            "显示应用配置、数据和缓存路径",
        ),
        ("config", "Configure via the TUI", "使用 TUI 进行配置"),
        ("reload", "Reload configuration", "重新加载配置"),
        (
            "models",
            "Switch the terminal-integration session's model",
            "修改终端集成会话的模型",
        ),
        ("list-models", "List available models", "列出可用模型"),
        (
            "variant",
            "Switch the terminal session model's thinking level",
            "切换终端集成会话模型的思考档位",
        ),
        (
            "fish-init",
            "Integrate with fish so you can chat in natural language directly in the terminal",
            "集成到 fish，集成后可在终端直接使用自然语言交流。",
        ),
        ("bash-init", "Integrate with bash", "集成到 bash"),
        ("zsh-init", "Integrate with zsh", "集成到 zsh"),
        (
            "remove-shell-hook",
            "Safely remove installed GQY shell hooks",
            "安全删除已安装的 GQY shell hook",
        ),
        ("history", "Show conversation history", "显示会话历史"),
        (
            "pop",
            "Move conversation turns out of active context",
            "将对话轮次移出当前上下文",
        ),
        ("kb", "Manage the knowledge base", "管理知识库"),
        (
            "update-default-kb",
            "Update GQY default knowledge base",
            "更新 GQY 默认知识库",
        ),
        ("memory", "Manage assistant memory", "管理记忆"),
        ("skills", "Manage assistant skills", "管理助手 skills"),
        (
            "platform",
            "Manage third-party communication platforms",
            "管理第三方通信平台",
        ),
        (
            "resources",
            "Show script/skill review & install status",
            "查看脚本/Skill 审查与安装状态",
        ),
        (
            "reset",
            "Clear the terminal-integration session context",
            "清除终端集成会话上下文",
        ),
        (
            "reset-memory",
            "Erase this persona's long-term memory",
            "清空长期记忆",
        ),
        (
            "wipe",
            "Erase all conversation history, memory, group contexts and their artifacts",
            "抹掉所有会话历史、记忆、群聊上下文和其产物",
        ),
        ("web", "Open the local GQY WebUI", "访问本地 GQY WebUI"),
        (
            "daemon",
            "Manage the unified GQY background service",
            "管理 GQY 统一后台服务",
        ),
        (
            "export",
            "Export configuration into a portable archive",
            "导出配置，把当前配置打包成可移植归档",
        ),
        ("import", "Import configuration", "导入配置"),
    ];
    for (name, en, zh) in descriptions {
        command = command.mut_subcommand(name, |subcommand| subcommand.about(t(en, zh)));
    }
    // 终端无缝集成组:从 {subcommands} 里藏掉,根帮助模板里以静态段
    // 单独成节(clap 不支持子命令分组);`gqy <cmd> -h` 不受影响。
    for name in [
        "fish-init",
        "bash-init",
        "zsh-init",
        "remove-shell-hook",
        "models",
        "variant",
        "history",
        "reset",
        "reset-memory",
        "pop",
    ] {
        command = command.mut_subcommand(name, |subcommand| subcommand.hide(true));
    }
    for (index, name) in [
        "init",
        "config",
        "normal",
        "dev",
        "daemon",
        "web",
        "tool-call",
        "ask",
        "list-models",
        "export",
        "import",
        "kb",
        "memory",
        "skills",
        "platform",
        "resources",
        "update-default-kb",
        "wipe",
        "paths",
        "reload",
    ]
    .into_iter()
    .enumerate()
    {
        command = command.mut_subcommand(name, move |subcommand| subcommand.display_order(index));
    }
    command = command
        .mut_subcommand("ask", localize_ask_command)
        .mut_subcommand("models", localize_models_command)
        .mut_subcommand("variant", localize_variant_command)
        .mut_subcommand("history", localize_history_command)
        .mut_subcommand("pop", localize_pop_command)
        .mut_subcommand("kb", localize_kb_command)
        .mut_subcommand("memory", localize_memory_command)
        .mut_subcommand("skills", localize_skills_command)
        .mut_subcommand("platform", localize_platform_command)
        .mut_subcommand("resources", localize_resources_command)
        .mut_subcommand("config", localize_config_command)
        .mut_subcommand("web", localize_web_command)
        .mut_subcommand("daemon", localize_daemon_command)
        .mut_subcommand("export", localize_export_command)
        .mut_subcommand("import", localize_import_command);
    command
}

pub(crate) fn localize_export_command(command: clap::Command) -> clap::Command {
    command
        .mut_arg("output", |arg| {
            arg.help(t(
                "Archive path to write; omit to name it after this host and time",
                "要写入的归档路径；省略则按主机名与时间自动命名",
            ))
        })
        .mut_arg("all", |arg| {
            arg.help(t(
                "Include everything portable, index and platform history included",
                "包含全部可移植数据，含向量索引与平台历史",
            ))
        })
        .mut_arg("index", |arg| {
            arg.help(t(
                "Include the knowledge-base vector index (large; rebuildable with `gqy kb embed`)",
                "包含知识库向量索引（很大；可用 gqy kb embed 重建）",
            ))
        })
        .mut_arg("platforms", |arg| {
            arg.help(t("Include chat-platform history", "包含通讯平台的聊天历史"))
        })
        .mut_arg("no_secrets", |arg| {
            arg.help(t(
                "Blank out API keys and tokens (you must refill them after importing)",
                "清空 API key 与访问令牌（导入后需要自行补填）",
            ))
        })
        .mut_arg("dry_run", |arg| {
            arg.help(t(
                "Print what would be packed without writing an archive",
                "只打印将要打包的内容，不实际写归档",
            ))
        })
        .mut_arg("force", |arg| {
            arg.help(t("Overwrite an existing archive", "覆盖已存在的归档文件"))
        })
}

pub(crate) fn localize_import_command(command: clap::Command) -> clap::Command {
    command
        .mut_arg("archive", |arg| {
            arg.help(t(
                "Archive produced by `gqy export`",
                "gqy export 生成的归档",
            ))
        })
        .mut_arg("force", |arg| {
            arg.help(t(
                "Overwrite existing data (the current installation is backed up first)",
                "覆盖已有数据（覆盖前会先备份当前安装）",
            ))
        })
}

pub(crate) fn localize_ask_command(command: clap::Command) -> clap::Command {
    command.mut_arg("message", |arg| {
        arg.help(t("Message to send", "要发送的消息"))
    })
}

pub(crate) fn localize_models_command(command: clap::Command) -> clap::Command {
    command.mut_arg("target", |arg| {
        arg.help(t(
            "List index, provider/model, or 'default' to follow the global pool",
            "模型列表序号、供应商/模型名，或 default 恢复跟随全局模型池",
        ))
    })
}

pub(crate) fn localize_variant_command(command: clap::Command) -> clap::Command {
    command.mut_arg("name", |arg| {
        arg.help(t(
            "Thinking level to select; omit to choose interactively",
            "要选择的思考档位；省略则进入交互选择",
        ))
    })
}

pub(crate) fn localize_history_command(command: clap::Command) -> clap::Command {
    command
        .mut_arg("limit", |arg| {
            arg.help(t("Number of history entries to show", "显示的历史条数"))
        })
        .mut_arg("raw", |arg| {
            arg.help(t("Print raw JSONL entries", "输出原始 JSONL 条目"))
        })
        .mut_arg("no_thinking", |arg| {
            arg.help(t("Hide stored reasoning", "隐藏已保存的思考内容"))
        })
}

pub(crate) fn localize_pop_command(command: clap::Command) -> clap::Command {
    command.mut_arg("count", |arg| {
        arg.help(t(
            "Number of oldest turns to pop; omit to select interactively",
            "要弹出的最旧轮次数；省略则进入交互多选",
        ))
    })
}

pub(crate) fn localize_config_command(command: clap::Command) -> clap::Command {
    command
        .mut_subcommand("validate", |subcommand| {
            subcommand.about(t("Validate configuration", "校验配置"))
        })
        .mut_subcommand("paths", |subcommand| {
            subcommand.about(t("Show configuration paths", "显示配置路径"))
        })
}

pub(crate) fn localize_web_command(command: clap::Command) -> clap::Command {
    command
        .mut_arg("port", |arg| arg.help(t("Local TCP port", "本地 TCP 端口")))
        .mut_arg("password", |arg| {
            arg.help(t(
                "Prompt securely for a required password",
                "安全输入所需的访问密码",
            ))
        })
        .mut_arg("password_file", |arg| {
            arg.help(t(
                "Read the WebUI password from a file",
                "从文件读取 WebUI 访问密码",
            ))
        })
}

pub(crate) fn localize_daemon_command(mut command: clap::Command) -> clap::Command {
    let descriptions = [
        (
            "start",
            "Start all configured GQY interfaces",
            "启动所有已配置的 GQY 接口",
        ),
        (
            "stop",
            "Stop the GQY background service",
            "停止 GQY 后台服务",
        ),
        (
            "restart",
            "Restart the GQY background service",
            "重启 GQY 后台服务",
        ),
        (
            "status",
            "Show daemon and interface status",
            "显示 daemon 与接口状态",
        ),
        ("logs", "Follow daemon logs", "持续查看 daemon 日志"),
    ];
    for (name, en, zh) in descriptions {
        command = command.mut_subcommand(name, |subcommand| subcommand.about(t(en, zh)));
    }
    command
        .mut_arg("port", |arg| {
            arg.help(t("WebUI TCP port", "WebUI TCP 端口"))
        })
        .mut_subcommand("logs", |subcommand| {
            subcommand.mut_arg("lines", |arg| {
                arg.help(t(
                    "Print only the most recent N lines and exit",
                    "仅输出最近 N 行后退出",
                ))
            })
        })
}

pub(crate) fn localize_kb_command(mut command: clap::Command) -> clap::Command {
    let descriptions = [
        ("add", "Add a file or directory", "添加文件或目录"),
        ("list", "List indexed files", "列出已索引文件"),
        ("search", "Search knowledge base content", "搜索知识库内容"),
        ("find", "Find files by name", "按文件名查找文件"),
        ("read", "Read a knowledge base file", "读取知识库文件"),
        ("remove", "Remove a knowledge base file", "移除知识库文件"),
        (
            "reindex",
            "Rebuild keyword index on demand",
            "按需重建关键词索引",
        ),
        ("stats", "Show knowledge base statistics", "显示知识库统计"),
        ("embed", "Manage semantic embeddings", "管理语义嵌入"),
    ];
    for (name, en, zh) in descriptions {
        command = command.mut_subcommand(name, |subcommand| subcommand.about(t(en, zh)));
    }
    command
        .mut_subcommand("add", |subcommand| {
            subcommand
                .mut_arg("path", |arg| arg.help(t("Path to add", "要添加的路径")))
                .mut_arg("recursive", |arg| {
                    arg.help(t(
                        "Compatibility flag; directories are recursive by default",
                        "兼容参数；目录默认递归导入",
                    ))
                })
        })
        .mut_subcommand("search", |subcommand| {
            subcommand
                .mut_arg("query", |arg| arg.help(t("Search query", "搜索查询")))
                .mut_arg("limit", |arg| arg.help(t("Maximum results", "最大结果数")))
        })
        .mut_subcommand("find", |subcommand| {
            subcommand
                .mut_arg("query", |arg| arg.help(t("Filename query", "文件名查询")))
                .mut_arg("limit", |arg| arg.help(t("Maximum results", "最大结果数")))
        })
        .mut_subcommand("read", |subcommand| {
            subcommand
                .mut_arg("file", |arg| {
                    arg.help(t("Knowledge base file name", "知识库文件名"))
                })
                .mut_arg("start", |arg| arg.help(t("Starting line", "起始行")))
                .mut_arg("lines", |arg| arg.help(t("Number of lines", "读取行数")))
        })
        .mut_subcommand("remove", |subcommand| {
            subcommand.mut_arg("file", |arg| arg.help(t("File to remove", "要移除的文件")))
        })
}

pub(crate) fn localize_memory_command(mut command: clap::Command) -> clap::Command {
    let descriptions = [
        ("stats", "Show memory statistics", "显示记忆统计"),
        ("reset", "Clear assistant memory", "清空助手记忆"),
        ("search", "Search memories", "搜索记忆"),
        ("remember", "Save a manual fact", "手动保存事实"),
    ];
    for (name, en, zh) in descriptions {
        command = command.mut_subcommand(name, |subcommand| subcommand.about(t(en, zh)));
    }
    command
        .mut_subcommand("reset", |subcommand| {
            subcommand.mut_arg("include_skills", |arg| {
                arg.help(t(
                    "Also remove generated skills",
                    "同时移除自动生成的 skills",
                ))
            })
        })
        .mut_subcommand("search", |subcommand| {
            subcommand
                .mut_arg("query", |arg| arg.help(t("Search query", "搜索查询")))
                .mut_arg("limit", |arg| arg.help(t("Maximum results", "最大结果数")))
                .mut_arg("forgotten", |arg| {
                    arg.help(t("Include forgotten memories", "包含已遗忘记忆"))
                })
        })
        .mut_subcommand("remember", |subcommand| {
            subcommand
                .mut_arg("content", |arg| arg.help(t("Fact content", "事实内容")))
                .mut_arg("source", |arg| arg.help(t("Source label", "来源标签")))
        })
}

pub(crate) fn localize_platform_command(mut command: clap::Command) -> clap::Command {
    let descriptions = [
        ("status", "Show all platform status", "查看所有平台状态"),
        ("list", "List registered platforms", "列出已注册平台"),
        ("show", "Show a platform's detail", "查看平台详情"),
        ("enable", "Enable a platform", "启用平台"),
        ("disable", "Disable a platform", "禁用平台"),
        ("restart", "Restart a platform", "重启平台"),
    ];
    for (name, en, zh) in descriptions {
        command = command.mut_subcommand(name, |subcommand| subcommand.about(t(en, zh)));
    }
    for name in ["show", "enable", "disable", "restart"] {
        command = command.mut_subcommand(name, |subcommand| {
            subcommand.mut_arg("name", |arg| arg.help(t("Platform id", "平台 id")))
        });
    }
    command
}
pub(crate) fn localize_skills_command(mut command: clap::Command) -> clap::Command {
    let descriptions = [
        ("list", "List skills", "列出 skills"),
        ("show", "Show a skill", "显示 skill"),
        ("enable", "Enable a skill", "启用 skill"),
        ("disable", "Disable a skill", "禁用 skill"),
        ("remove", "Remove a skill", "移除 skill"),
        ("stats", "Show skill statistics", "显示 skill 统计"),
        (
            "prune",
            "Remove disabled generated skills",
            "清理已禁用的自动 skills",
        ),
        (
            "import",
            "Import a skill directory (review first, then install)",
            "从本地路径导入 skill 目录（先审查后安装）",
        ),
    ];
    for (name, en, zh) in descriptions {
        command = command.mut_subcommand(name, |subcommand| subcommand.about(t(en, zh)));
    }
    for name in ["show", "enable", "disable", "remove"] {
        command = command.mut_subcommand(name, |subcommand| {
            subcommand.mut_arg("name", |arg| arg.help(t("Skill name", "skill 名称")))
        });
    }
    command = command.mut_subcommand("import", |subcommand| {
        subcommand.mut_arg("path", |arg| {
            arg.help(t(
                "Skill directory path (must contain SKILL.md)",
                "Skill 目录路径（必须包含 SKILL.md）",
            ))
        })
    });
    command
}

pub(crate) fn localize_resources_command(mut command: clap::Command) -> clap::Command {
    let descriptions = [
        (
            "status",
            "Show script/skill review & install status",
            "查看脚本/Skill 的审查与安装状态",
        ),
        (
            "prune",
            "Prune expired review/install records",
            "清理过期的审查与安装记录",
        ),
    ];
    for (name, en, zh) in descriptions {
        command = command.mut_subcommand(name, |subcommand| subcommand.about(t(en, zh)));
    }
    command
}

#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(name = "__alarm-worker", hide = true)]
    AlarmWorker(AlarmWorkerArgs),
    #[command(name = "__tool", hide = true)]
    Tool(ToolArgs),
    /// Internal: run as the GQY daemon (spawned by the CLI via
    /// `current_exe`, replacing the former separate `gqyd` binary).
    #[command(name = "__daemon", hide = true)]
    DaemonWorker(WebArgs),
    Ask(MessageArgs),
    Init,
    Paths,
    Config(ConfigArgs),
    Reload,
    Models(ModelsArgs),
    ListModels,
    Variant(VariantArgs),
    FishInit,
    BashInit,
    ZshInit,
    RemoveShellHook,
    History(HistoryArgs),
    Pop(PopArgs),
    Kb(KbArgs),
    Export(ExportArgs),
    Import(ImportArgs),
    UpdateDefaultKb,
    Memory(MemoryArgs),
    Skills(SkillsArgs),
    Resources(ResourcesArgs),
    /// 第三方通信平台管理（状态 / 启用 / 禁用 / 重启）。
    Platform(PlatformArgs),
    Reset,
    #[command(name = "reset-memory")]
    ResetMemoryCli,
    Wipe(WipeArgs),
    Web(WebArgs),
    Daemon(DaemonArgs),
    /// 进入普通模式 REPL(人格全能力)
    Normal,
    /// 进入开发模式 REPL(极简编码形态,无人格)
    Dev,
    /// 工具桥:以当前会话身份调用一个结构化工具(供 run_command 脚本编排)
    #[command(name = "tool-call")]
    ToolCallCmd(ToolCallArgs),
}

#[derive(Debug, Args)]
pub struct MessageArgs {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub message: Vec<String>,
}

#[derive(Debug, Args)]
pub struct WipeArgs {
    /// 跳过确认（供 shell hook 等非交互场景使用）。
    #[arg(long)]
    pub yes: bool,
}

#[derive(Args)]
pub struct WebArgs {
    #[arg(long, default_value_t = ipc::DEFAULT_WEB_PORT)]
    pub port: u16,

    /// WebUI 监听地址；默认 127.0.0.1（仅本机访问），0.0.0.0 暴露到局域网（建议配合 -p 设置密码）。
    #[arg(long, value_name = "ADDR")]
    pub bind: Option<std::net::IpAddr>,

    #[arg(short = 'p', long, num_args = 0, default_missing_value = "")]
    pub password: Option<String>,

    #[arg(long, value_name = "PATH", conflicts_with = "password")]
    pub password_file: Option<PathBuf>,

    #[arg(skip)]
    pub port_explicit: bool,
}

#[derive(Debug, Args)]
pub struct DaemonArgs {
    #[arg(long, value_name = "PORT", global = true)]
    pub port: Option<u16>,

    #[command(subcommand)]
    pub command: Option<DaemonCommand>,
}

#[derive(Debug, Subcommand)]
pub enum DaemonCommand {
    Start,
    Stop,
    Restart,
    Status,
    Logs(DaemonLogsArgs),
}

#[derive(Debug, Args)]
pub struct DaemonLogsArgs {
    #[arg(short = 'n', long, value_name = "N")]
    pub lines: Option<usize>,

    /// `request`:开启出网请求录制并实时监控;Ctrl+C 停止并关闭录制
    #[arg(value_name = "TOPIC")]
    pub topic: Option<String>,
}

impl std::fmt::Debug for WebArgs {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebArgs")
            .field("port", &self.port)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("password_file", &self.password_file)
            .field("port_explicit", &self.port_explicit)
            .finish()
    }
}

#[derive(Debug, Args)]
pub struct AlarmWorkerArgs {
    #[arg(long)]
    pub id: String,
    #[arg(long)]
    pub time: String,
    #[arg(long, default_value = "GQY alarm")]
    pub label: String,
    #[arg(long)]
    pub state_dir: PathBuf,
    #[arg(long)]
    pub cache_dir: PathBuf,
    #[arg(long)]
    pub audio_file: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct ToolArgs {
    pub name: String,
    pub arguments: Option<String>,
}

#[derive(Debug, Args)]
pub struct ToolCallArgs {
    /// 工具名(--list 时可省略)
    pub name: Option<String>,
    /// 参数 JSON(便捷位置参数;脚本里推荐 --stdin 免引号地狱)
    pub arguments: Option<String>,
    /// 从标准输入读参数 JSON(跨 shell 安全,PowerShell 也能用)
    #[arg(long = "stdin")]
    pub args_stdin: bool,
    /// 从文件读参数 JSON
    #[arg(long)]
    pub args_file: Option<std::path::PathBuf>,
    /// 列出当前可用工具(名称+显示名)
    #[arg(long)]
    pub list: bool,
    /// 打印指定工具的完整合同(描述+参数 schema)
    #[arg(long)]
    pub describe: bool,
}

#[derive(Debug, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: Option<ConfigCommand>,
}

#[derive(Debug, Args)]
pub struct HistoryArgs {
    #[arg(short, long, default_value_t = 20)]
    pub limit: usize,

    #[arg(long)]
    pub raw: bool,

    #[arg(long)]
    pub no_thinking: bool,
}

#[derive(Debug, Args)]
pub struct PopArgs {
    #[arg(value_parser = parse_positive_pop_count)]
    pub count: Option<usize>,
}

#[derive(Debug, Args)]
pub struct ExportArgs {
    /// Where to write the archive; defaults to a host- and time-stamped name
    /// in the current directory.
    pub output: Option<PathBuf>,
    #[arg(long)]
    pub all: bool,
    #[arg(long)]
    pub index: bool,
    #[arg(long)]
    pub platforms: bool,
    #[arg(long)]
    pub no_secrets: bool,
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct ImportArgs {
    pub archive: PathBuf,
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct ModelsArgs {
    /// 1-based list index, `provider/model`, a bare model name, or
    /// `default` to follow the global active pool again.
    pub target: Option<String>,
}

#[derive(Debug, Args)]
pub struct VariantArgs {
    pub name: Option<String>,
}

#[derive(Debug, Args)]
pub struct KbArgs {
    #[command(subcommand)]
    pub command: KbCommand,
}

#[derive(Debug, Args)]
pub struct MemoryArgs {
    #[command(subcommand)]
    pub command: MemoryCommand,
}

#[derive(Debug, Subcommand)]
pub enum MemoryCommand {
    Stats,
    Reset(MemoryResetArgs),
    Search(MemorySearchArgs),
    Remember(MemoryRememberArgs),
}

#[derive(Debug, Args)]
pub struct MemoryResetArgs {
    #[arg(long)]
    pub include_skills: bool,
}

#[derive(Debug, Args)]
pub struct MemorySearchArgs {
    pub query: Vec<String>,
    #[arg(short, long)]
    pub limit: Option<usize>,
    #[arg(long)]
    pub forgotten: bool,
}

#[derive(Debug, Args)]
pub struct MemoryRememberArgs {
    pub content: Vec<String>,
    #[arg(short, long, default_value = "manual")]
    pub source: String,
}

#[derive(Debug, Args)]
pub struct SkillsArgs {
    #[command(subcommand)]
    pub command: SkillsCommand,
}

#[derive(Debug, Args)]
pub struct ResourcesArgs {
    #[command(subcommand)]
    pub command: ResourcesCommand,
}

#[derive(Debug, Args)]
pub struct PlatformArgs {
    #[command(subcommand)]
    pub command: PlatformCommand,
}

#[derive(Debug, Subcommand)]
pub enum PlatformCommand {
    /// 查看所有第三方通信平台状态。
    Status,
    /// 列出已注册的第三方通信平台。
    List,
    /// 查看指定平台的详细状态（如 `gqy platform show qq`）。
    Show(PlatformNameArgs),
    /// 启用指定平台。
    Enable(PlatformNameArgs),
    /// 禁用指定平台。
    Disable(PlatformNameArgs),
    /// 重启指定平台（要求 daemon 运行中）。
    Restart(PlatformNameArgs),
}

#[derive(Debug, Args)]
pub struct PlatformNameArgs {
    /// 平台 id（如 `qq`）。
    pub name: String,
}

#[derive(Debug, Subcommand)]
pub enum SkillsCommand {
    List,
    Show(SkillNameArgs),
    Enable(SkillNameArgs),
    Disable(SkillNameArgs),
    Remove(SkillNameArgs),
    Stats,
    Prune,
    /// 从本地路径导入 skill 目录(先审查后安装)。
    #[command(name = "import")]
    Import(SkillImportArgs),
}

#[derive(Debug, Args)]
pub struct SkillImportArgs {
    /// 要导入的 skill 目录绝对路径(含 SKILL.md)。
    pub path: PathBuf,
    /// 跳过 AI 审查与用户确认,直接安装(危险,不推荐)。
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Subcommand)]
pub enum ResourcesCommand {
    /// 查看脚本/Skill 的审查与安装状态。
    Status,
    /// 清理过期的审查与安装记录。
    Prune,
}

#[derive(Debug, Args)]
pub struct SkillNameArgs {
    pub name: String,
}

#[derive(Debug, Subcommand)]
pub enum KbCommand {
    Add(KbAddArgs),
    List,
    Search(KbSearchArgs),
    Find(KbFindArgs),
    Read(KbReadArgs),
    Remove(KbRemoveArgs),
    Reindex,
    Stats,
    Embed(KbEmbedArgs),
}

#[derive(Debug, Args)]
pub struct KbAddArgs {
    pub path: PathBuf,
    #[arg(
        short,
        long,
        help = "Compatibility flag; directories are recursive by default"
    )]
    pub recursive: bool,
}

#[derive(Debug, Args)]
pub struct KbSearchArgs {
    pub query: Vec<String>,
    #[arg(short, long)]
    pub limit: Option<usize>,
}

#[derive(Debug, Args)]
pub struct KbFindArgs {
    pub query: Vec<String>,
    #[arg(short, long)]
    pub limit: Option<usize>,
}

#[derive(Debug, Args)]
pub struct KbReadArgs {
    pub file: String,
    #[arg(long, default_value_t = 1)]
    pub start: usize,
    #[arg(long)]
    pub lines: Option<usize>,
}
