//! crates/ramaria-cli/src/dispatch.rs - CLI 子命令分发模块
//!
//! 设计特点:
//! - 将 clap 解析结果映射到 `commands::*` 用例并驱动执行
//! - 子命令参数在此转换为服务层命令结构（如 `--no-rebuild-utt` → `rebuild_utt=false`）
//! - 全局 `--json` / `--yes` 经此透传到命令模块，交互式与批量行为一致
//! - 参数校验类错误以 `RamariaError::Validation` 返回，由入口统一映射退出码

use std::path::PathBuf;
use std::sync::Arc;

use ramaria_cli::commands;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_eval::{EvalCommand, EvalContext, SandboxBackendChoice, SandboxModelKind};
use ramaria_service::Engine;

use crate::cli::{
    BackendModeArg, BlocksCmd, ChannelCmd, Cli, Commands, ConfigCmd, CorpusCmd, EvalArgs, EvalCmd,
    FactCmd, ImportCmd, IndexCmd, KeywordAliasCmd, KeywordCmd, PersonaCmd, ProbeArgs, RuleCmd,
    SessionCmd, StickerCmd, StyleCmd,
};

/// 评测参数 → 评测层命令（纯映射，不触碰引擎与文件系统）。
///
/// 参数:
/// - `args`: clap 解析出的评测参数组。
///
/// 返回:
/// - `None`: 未给子命令（进入沙盒界面）；
/// - `Some`: 对应评测命令，语义校验留给评测层。
pub(crate) fn eval_command(args: &EvalArgs) -> Option<EvalCommand> {
    let command = args.command.as_ref()?;
    let mapped = match command {
        EvalCmd::Run {
            corpus,
            matrix,
            output,
        } => EvalCommand::Run {
            corpus: corpus.clone(),
            matrix: matrix.clone(),
            output: output.clone(),
        },
        EvalCmd::Corpus(CorpusCmd::Verify { name, language }) => EvalCommand::CorpusVerify {
            name: name.clone(),
            language: language.clone(),
        },
        EvalCmd::Baseline {
            corpus,
            tier,
            language,
            output,
        } => EvalCommand::Baseline {
            corpus: corpus.clone(),
            tier: tier.clone(),
            language: language.clone(),
            output: output.clone(),
        },
        EvalCmd::Score { run_id } => EvalCommand::Score {
            run_id: run_id.clone(),
        },
        EvalCmd::Report { run_id, output } => EvalCommand::Report {
            run_id: run_id.clone(),
            output: output.clone(),
        },
    };
    Some(mapped)
}

/// 评测参数 → 评测运行上下文（后端来源与用户库路径）。
///
/// 参数:
/// - `args`: clap 解析出的评测参数组；
/// - `user_db`: 用户库路径，供继承来源读取后端配置。
///
/// 返回:
/// - 无子命令时返回本次沙盒的后端来源选择；
/// - 子命令带后端参数、或继承来源带独立配置参数时返回 `Validation`。
pub(crate) fn eval_context(args: &EvalArgs, user_db: PathBuf) -> RamariaResult<EvalContext> {
    let has_independent =
        args.base_url.is_some() || args.model.is_some() || args.embedding_path.is_some();

    if args.command.is_some() {
        if args.backend != BackendModeArg::Inherit || has_independent {
            return Err(RamariaError::validation(
                "eval 子命令不接受后端参数：后端来源只对无子命令的沙盒生效",
            ));
        }
        return Ok(EvalContext {
            user_db: Some(user_db),
            backend: SandboxBackendChoice::Inherit,
        });
    }

    let backend = match args.backend {
        BackendModeArg::Inherit => {
            if has_independent {
                return Err(RamariaError::validation(
                    "继承模式不接受独立配置参数：先用 --backend local 或 --backend cloud",
                ));
            }
            SandboxBackendChoice::Inherit
        }
        BackendModeArg::Local | BackendModeArg::Cloud => {
            let kind = match args.backend {
                BackendModeArg::Local => SandboxModelKind::Local,
                _ => SandboxModelKind::Cloud,
            };
            SandboxBackendChoice::Independent {
                kind,
                base_url: args.base_url.clone().unwrap_or_default(),
                model_id: args.model.clone().unwrap_or_default(),
                embedding_model_path: args.embedding_path.clone(),
            }
        }
    };

    Ok(EvalContext {
        user_db: Some(user_db),
        backend,
    })
}

// =========================================================
// 命令调度
// =========================================================

pub(crate) async fn dispatch(engine: &Arc<Engine>, cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Commands::Setup(args) => {
            let options = commands::setup::SetupOptions {
                provider: args.provider,
                base_url: args.base_url,
                model: args.model,
                api_key: args.api_key,
                embedding_path: args.embedding_path,
                no_validate: args.no_validate,
            };
            commands::setup::run(engine, options).await?;
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
                PersonaCmd::Import {
                    file,
                    dry_run,
                    into,
                    no_ai,
                } => commands::persona::PersonaCmd::Import {
                    file,
                    dry_run,
                    into,
                    no_ai,
                },
                PersonaCmd::AllowInference { uid, label } => {
                    commands::persona::PersonaCmd::AllowInference { uid, labels: label }
                }
                PersonaCmd::Prompt { uid, input } => {
                    commands::persona::PersonaCmd::Prompt { uid, input }
                }
                PersonaCmd::Export {
                    uid,
                    out,
                    with_examples,
                } => commands::persona::PersonaCmd::Export {
                    uid,
                    out,
                    with_examples,
                },
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
        Commands::Channel(sub) => {
            let cmd = match sub {
                ChannelCmd::Status => commands::channel::ChannelCmd::Status,
                ChannelCmd::Bindings => commands::channel::ChannelCmd::Bindings,
                ChannelCmd::Bind {
                    channel,
                    external_ref,
                    persona,
                    enable,
                    disable,
                    allow_proactive,
                    deny_proactive,
                } => commands::channel::ChannelCmd::Bind {
                    channel,
                    external_ref,
                    persona,
                    enable,
                    disable,
                    allow_proactive,
                    deny_proactive,
                },
            };
            commands::channel::run(engine, cmd, cli.json).await?;
        }
        Commands::Sticker(sub) => {
            let cmd = match sub {
                StickerCmd::Add {
                    file,
                    description,
                    tags,
                } => commands::sticker::StickerCmd::Add {
                    file,
                    description,
                    tags,
                },
                StickerCmd::List => commands::sticker::StickerCmd::List,
                StickerCmd::Update {
                    id,
                    description,
                    tags,
                } => commands::sticker::StickerCmd::Update {
                    id,
                    description,
                    tags,
                },
                StickerCmd::Remove { id } => commands::sticker::StickerCmd::Remove { id },
            };
            commands::sticker::run(engine, cmd, cli.json).await?;
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
            ImportCmd::Continue {
                export_root,
                no_reset_skipped,
                limit,
                no_cascade,
                yes,
            } => {
                let args = commands::import_cmd::ContinueArgs {
                    export_roots: export_root,
                    reset_skipped: !no_reset_skipped,
                    limit,
                    cascade_deep: !no_cascade,
                    // 全局 --yes 与子命令 --yes 任一生效
                    yes: cli.yes || yes,
                    json: cli.json,
                };
                commands::import_cmd::run_continue(engine, args).await?;
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
                ProbeArgs::Baseline { window_hours } => commands::probe::ProbeCmd::Baseline {
                    window_hours,
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
        // eval 在引擎初始化前分流（见 main：评测不建用户库与配置）；此处仅保证穷尽
        Commands::Eval(_) => {
            return Err(anyhow::anyhow!(RamariaError::unsupported(
                "eval 命令应在引擎初始化前分流（内部错误）"
            )));
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造只带子命令的评测参数，其余字段取缺省。
    fn args(command: Option<EvalCmd>) -> EvalArgs {
        EvalArgs {
            backend: BackendModeArg::Inherit,
            base_url: None,
            model: None,
            embedding_path: None,
            command,
        }
    }

    /// 无参数时继承当前配置，并带上用户库路径。
    #[test]
    fn eval_context_defaults_to_inherit() {
        let context = eval_context(&args(None), PathBuf::from("u.db")).expect("应构造成功");
        assert_eq!(context.user_db, Some(PathBuf::from("u.db")));
        assert_eq!(context.backend, SandboxBackendChoice::Inherit);
    }

    /// 本地来源允许空地址与空模型，交给沙盒默认地址与自动选择。
    #[test]
    fn eval_context_maps_local_without_address_or_model() {
        let mut eval = args(None);
        eval.backend = BackendModeArg::Local;
        let context = eval_context(&eval, PathBuf::from("u.db")).expect("应构造成功");
        assert_eq!(
            context.backend,
            SandboxBackendChoice::Independent {
                kind: SandboxModelKind::Local,
                base_url: String::new(),
                model_id: String::new(),
                embedding_model_path: None,
            }
        );
    }

    /// 云端来源透传地址、模型与向量目录。
    #[test]
    fn eval_context_maps_cloud_independent_fields() {
        let mut eval = args(None);
        eval.backend = BackendModeArg::Cloud;
        eval.base_url = Some("https://cloud.example/v1".to_string());
        eval.model = Some("model-a".to_string());
        eval.embedding_path = Some(PathBuf::from("models/bge"));
        let context = eval_context(&eval, PathBuf::from("u.db")).expect("应构造成功");
        assert_eq!(
            context.backend,
            SandboxBackendChoice::Independent {
                kind: SandboxModelKind::Cloud,
                base_url: "https://cloud.example/v1".to_string(),
                model_id: "model-a".to_string(),
                embedding_model_path: Some(PathBuf::from("models/bge")),
            }
        );
    }

    /// 语料校验参数原样映射到评测层命令。
    #[test]
    fn eval_command_maps_corpus_verify() {
        let eval = args(Some(EvalCmd::Corpus(CorpusCmd::Verify {
            name: "lifebench".to_string(),
            language: Some("zh".to_string()),
        })));
        assert_eq!(
            eval_command(&eval),
            Some(EvalCommand::CorpusVerify {
                name: "lifebench".to_string(),
                language: Some("zh".to_string()),
            })
        );
    }

    /// 继承来源与独立配置参数互斥。
    #[test]
    fn eval_context_rejects_independent_options_in_inherit_mode() {
        let mut eval = args(None);
        eval.model = Some("model-a".to_string());
        let error = eval_context(&eval, PathBuf::from("u.db")).expect_err("应拒绝");
        assert!(matches!(&error, RamariaError::Validation { .. }));
    }

    /// 子命令不接受后端参数：后端来源只服务无子命令沙盒。
    #[test]
    fn eval_context_rejects_backend_options_with_subcommand() {
        let mut eval = args(Some(EvalCmd::Score {
            run_id: "r".to_string(),
        }));
        eval.backend = BackendModeArg::Cloud;
        let error = eval_context(&eval, PathBuf::from("u.db")).expect_err("应拒绝");
        assert!(matches!(&error, RamariaError::Validation { .. }));
    }
}
