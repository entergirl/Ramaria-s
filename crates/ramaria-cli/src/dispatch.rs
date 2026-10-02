//! crates/ramaria-cli/src/dispatch.rs - CLI 子命令分发模块
//!
//! 设计特点:
//! - 将 clap 解析结果映射到 `commands::*` 用例并驱动执行
//! - 子命令参数在此转换为服务层命令结构（如 `--no-rebuild-utt` → `rebuild_utt=false`）
//! - 全局 `--json` / `--yes` 经此透传到命令模块，交互式与批量行为一致
//! - 参数校验类错误以 `RamariaError::Validation` 返回，由入口统一映射退出码

use std::sync::Arc;

use ramaria_cli::commands;
use ramaria_core::error::RamariaError;
use ramaria_service::Engine;

use crate::cli::{
    BlocksCmd, Cli, Commands, ConfigCmd, FactCmd, ImportCmd, IndexCmd, KeywordAliasCmd, KeywordCmd,
    PersonaCmd, ProbeArgs, RuleCmd, SessionCmd, StyleCmd,
};

// =========================================================
// 命令调度
// =========================================================

pub(crate) async fn dispatch(engine: &Arc<Engine>, cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Commands::Setup => {
            commands::setup::run(engine, cli.skip_validate).await?;
        }
        Commands::Ask {
            message,
            persona,
            session,
            no_stream,
            json,
        } => {
            let msg = message.join(" ");
            if msg.trim().is_empty() {
                // 业务校验失败
                return Err(anyhow::anyhow!(RamariaError::validation(
                    "消息不能为空。用法: ramaria ask <消息>"
                )));
            }

            let args = commands::ask::AskArgs {
                message: msg,
                persona,
                session,
                no_stream,
                // 子命令 --json 与全局 --json 等价（任一开启即事件流输出）
                json: cli.json || json,
                yes: cli.yes,
            };
            commands::ask::run(engine, args).await?;
        }
        Commands::Chat => {
            commands::chat::run(engine, cli.yes).await?;
        }
        Commands::Memory {
            layer,
            persona,
            limit,
            offset,
        } => {
            let args = commands::memory::MemoryArgs {
                layer,
                persona,
                limit,
                offset,
                json: cli.json,
            };
            commands::memory::run(engine, args).await?;
        }
        Commands::Blocks(sub) => match sub {
            BlocksCmd::Rebuild { force } => {
                commands::utt::run(engine, commands::utt::UttCmd::Rebuild { force }, cli.json)
                    .await?;
            }
        },
        Commands::Index(sub) => match sub {
            IndexCmd::Rebuild => {
                commands::index_cmd::run(engine, cli.json).await?;
            }
        },
        Commands::Session(sub) => {
            let cmd = match sub {
                SessionCmd::List { limit, offset } => {
                    commands::session::SessionCmd::List { limit, offset }
                }
                SessionCmd::Show { session_id } => {
                    commands::session::SessionCmd::Show { session_id }
                }
                SessionCmd::Delete { session_id, force } => {
                    commands::session::SessionCmd::Delete { session_id, force }
                }
                SessionCmd::Summarize {
                    session_id,
                    persona,
                    progressive,
                } => commands::session::SessionCmd::Summarize {
                    session_id,
                    persona_uid: persona,
                    progressive,
                },
            };
            commands::session::run(engine, cmd, cli.json, cli.yes).await?;
        }
        Commands::Config(sub) => {
            let cmd = match sub {
                ConfigCmd::List => commands::config::ConfigCmd::List,
                ConfigCmd::Get { key } => commands::config::ConfigCmd::Get { key },
                ConfigCmd::Set { key, value } => commands::config::ConfigCmd::Set { key, value },
            };
            commands::config::run(engine, cmd, cli.json).await?;
        }
        Commands::Persona(sub) => {
            let cmd = match sub {
                PersonaCmd::List { limit, offset } => {
                    commands::persona::PersonaCmd::List { limit, offset }
                }
                PersonaCmd::Show => commands::persona::PersonaCmd::Show,
                PersonaCmd::Reload { uid } => commands::persona::PersonaCmd::Reload { uid },
            };
            commands::persona::run(engine, cmd, cli.json).await?;
        }
        Commands::Rule(sub) => {
            let cmd = match sub {
                RuleCmd::List {
                    persona,
                    limit,
                    offset,
                } => commands::rule::RuleCmd::List {
                    persona,
                    limit: Some(limit),
                    offset,
                },
                RuleCmd::Show { id } => commands::rule::RuleCmd::Show { id },
                RuleCmd::Import { file, persona } => {
                    commands::rule::RuleCmd::Import { file, persona }
                }
                RuleCmd::Edit {
                    id,
                    reaction,
                    avoid,
                } => commands::rule::RuleCmd::Edit {
                    id,
                    reaction,
                    avoid,
                },
                RuleCmd::Enable { id } => commands::rule::RuleCmd::Enable { id },
                RuleCmd::Disable { id } => commands::rule::RuleCmd::Disable { id },
                RuleCmd::Delete { id, force } => commands::rule::RuleCmd::Delete { id, force },
                RuleCmd::Evidence { id } => commands::rule::RuleCmd::Evidence { id },
                RuleCmd::Relearn { persona } => commands::rule::RuleCmd::Relearn { persona },
                RuleCmd::Clusters {
                    persona,
                    theta_nb,
                    min_cluster_size,
                    beta1,
                    beta2,
                    theta_join,
                    split_ratio,
                } => commands::rule::RuleCmd::Clusters {
                    persona,
                    theta_nb,
                    min_cluster_size,
                    beta1,
                    beta2,
                    theta_join,
                    split_ratio,
                },
            };
            commands::rule::run(engine, cmd, cli.json, cli.yes).await?;
        }
        Commands::Style(sub) => {
            let cmd = match sub {
                StyleCmd::Update { persona } => commands::style::StyleCmd::Update { persona },
            };
            commands::style::run(engine, cmd, cli.json).await?;
        }
        Commands::Fact(sub) => {
            let cmd = match sub {
                FactCmd::List {
                    persona,
                    field,
                    limit,
                    offset,
                } => commands::fact::FactCmd::List {
                    persona,
                    field,
                    limit: Some(limit),
                    offset,
                },
                FactCmd::Show { id } => commands::fact::FactCmd::Show { id },
            };
            commands::fact::run(engine, cmd, cli.json).await?;
        }
        Commands::Keyword(sub) => {
            let cmd = match sub {
                KeywordCmd::List => commands::keyword_cmd::KeywordCmd::List,
                KeywordCmd::Show { keyword } => commands::keyword_cmd::KeywordCmd::Show { keyword },
                KeywordCmd::Seed { keyword } => {
                    commands::keyword_cmd::KeywordCmd::Seed { keywords: keyword }
                }
                KeywordCmd::Alias(alias_sub) => {
                    let action = match alias_sub {
                        KeywordAliasCmd::List => commands::keyword_cmd::AliasAction::List,
                        KeywordAliasCmd::Confirm { alias } => {
                            commands::keyword_cmd::AliasAction::Confirm { alias }
                        }
                        KeywordAliasCmd::Reject { alias } => {
                            commands::keyword_cmd::AliasAction::Reject { alias }
                        }
                    };
                    commands::keyword_cmd::KeywordCmd::Alias(action)
                }
            };
            // keyword 命令经服务层关键词用例访问 keyword_pool
            commands::keyword_cmd::run(engine, cmd, cli.json, cli.yes).await?;
        }
        Commands::Export {
            format,
            persona,
            output,
            redact,
        } => {
            let args = commands::export::ExportArgs {
                format,
                persona,
                output,
                redact,
                json: cli.json,
            };
            commands::export::run(engine, args).await?;
        }
        Commands::Import(sub) => match sub {
            ImportCmd::Qq {
                file,
                deep,
                force,
                dry_run,
                persona,
                persona_self_name,
                persona_self_uid,
                persona_other_name,
                persona_other_uid,
                side,
                gap,
                no_report,
            } => {
                // --persona 向后兼容（映射为 self_name）
                let effective_self_name = persona_self_name.or(persona);
                let args = commands::import_cmd::ImportArgs {
                    file,
                    deep,
                    dry_run,
                    persona_self_name: effective_self_name,
                    persona_self_uid,
                    persona_other_name,
                    persona_other_uid,
                    gap,
                    side,
                    no_report,
                    // --force 与 --yes 双保险
                    yes: cli.yes || force,
                    json: cli.json,
                };
                commands::import_cmd::run(engine, args).await?;
            }
        },
        Commands::Diagnostics { output, redact } => {
            let args = commands::diagnostics::DiagnosticsArgs { output, redact };
            commands::diagnostics::run(engine, args, cli.json).await?;
        }
        Commands::Status => {
            let args = commands::status::StatusArgs {
                db_path: cli.db,
                json: cli.json,
            };
            commands::status::run(engine, args).await?;
        }
        Commands::Probe(sub) => {
            let cmd = match sub {
                ProbeArgs::Build {
                    persona,
                    questions_per_dim,
                    seed,
                    source,
                    output,
                    ablation,
                } => commands::probe::ProbeCmd::Build {
                    persona,
                    questions_per_dim,
                    seed,
                    source,
                    output,
                    ablation,
                    json: cli.json,
                },
                ProbeArgs::Run {
                    dataset,
                    variants,
                    limit,
                    no_rebuild_utt,
                    repeat,
                    output,
                } => commands::probe::ProbeCmd::Run {
                    dataset,
                    variants,
                    limit,
                    // clap 的 --no-rebuild-utt（默认 false）；内部 rebuild_utt=true 表示重建
                    rebuild_utt: !no_rebuild_utt,
                    repeat: (repeat > 1).then_some(repeat),
                    output,
                    json: cli.json,
                },
                ProbeArgs::Evaluate {
                    results,
                    dataset,
                    variants,
                    output,
                    no_tone_judge,
                } => commands::probe::ProbeCmd::Evaluate {
                    results,
                    dataset,
                    variants,
                    output,
                    no_tone_judge,
                    json: cli.json,
                },
                ProbeArgs::Report {
                    results,
                    evaluation,
                    calibration,
                    output,
                    ablation,
                } => commands::probe::ProbeCmd::Report {
                    results,
                    evaluation,
                    calibration,
                    output,
                    ablation,
                    json: cli.json,
                },
            };
            // 探针命令经服务层引擎执行（部分算法原语由探针模块直接调用）
            commands::probe::run(engine, cmd, cli.yes).await?;
        }
        // mcp 在引擎初始化前分流（见 main：直接走服务层引擎）；此处仅保证穷尽
        Commands::Mcp(_) => {
            return Err(anyhow::anyhow!(RamariaError::unsupported(
                "mcp 命令应在引擎初始化前分流（内部错误）"
            )));
        }
    }

    Ok(())
}
