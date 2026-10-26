/**
 * src/core/api/commands.ts — Tauri 命令表（单一源）
 *
 * 职责:
 * - 声明前端可用的全部命令：域 → 方法名 → Rust 命令名
 * - 作为 `check_contract.py` 的解析对象，与 Rust 注册表做机器校验
 *
 * 设计特点:
 * - 全仓唯一允许出现命令字符串的位置，其余代码经域方法调用
 * - 方法名为前端 camelCase，命令名为 Rust snake_case，逐条一一对应
 * - 表体保持「一行一条 `方法: '命令',`」格式，供正则稳定提取
 */

export const COMMANDS = {
  chat: {
    send: 'send_message',
    save: 'save_current_session',
    generateL1: 'generate_l1',
    getAppState: 'get_app_state',
    checkPrivacy: 'check_privacy',
    confirmPrivacy: 'confirm_privacy',
    recallMessage: 'recall_message',
    sendLocalImage: 'send_local_image',
  },
  setup: {
    run: 'run_setup',
    refresh: 'refresh_setup_state',
    testLlmConnection: 'test_llm_connection',
    validateEmbeddingModel: 'validate_embedding_model',
    saveEmbeddingModel: 'save_embedding_model',
    getEmbeddingModel: 'get_embedding_model',
    getDegradedReason: 'get_degraded_reason',
    setVisionSupport: 'set_vision_support',
    downloadEmbeddingModel: 'download_embedding_model',
  },
  session: {
    list: 'list_sessions',
    get: 'get_session',
    create: 'create_session',
    markRead: 'mark_session_read',
  },
  memory: {
    getPersonas: 'get_personas',
    getL1: 'get_l1_memories',
    getL2: 'get_l2_events',
    getProfile: 'get_personality_profile',
    getEvidence: 'get_trait_evidence',
    getProfileStatus: 'get_profile_status',
    getFacts: 'get_facts',
    triggerPipeline: 'trigger_memory_pipeline',
  },
  config: {
    getBackend: 'get_backend_config',
    updateBackend: 'update_backend_config',
    getSettings: 'get_settings',
    updateSetting: 'update_setting',
    getFull: 'get_full_config',
    updateFull: 'update_full_config',
    getDefaults: 'get_default_config',
  },
  mcp: {
    getInfo: 'get_mcp_info',
  },
  export: {
    json: 'export_sessions_json',
    markdown: 'export_sessions_markdown',
  },
  index: {
    getStatus: 'get_index_status',
    rebuild: 'rebuild_index',
  },
  import: {
    detectFormat: 'detect_qq_format',
    analyzeFile: 'analyze_qq_chat',
    importQQ: 'import_qq_chat',
    continueImport: 'continue_import',
    getStatus: 'get_import_status',
  },
  persona: {
    listFull: 'list_personas_full',
    updateInfo: 'update_persona_info',
    refresh: 'refresh_persona',
    regenerateImportPipeline: 'regenerate_import_pipeline',
    previewPrompt: 'preview_persona_prompt',
    previewImport: 'preview_persona_import',
    importPreset: 'import_persona_preset',
    personaExport: 'persona_export',
  },
  proactive: {
    listPersonas: 'list_proactive_personas',
    setPersona: 'set_proactive_persona',
    getStats: 'get_proactive_stats',
  },
  channel: {
    getStatus: 'get_channel_status',
    start: 'start_channel',
    stop: 'stop_channel',
    listBindings: 'list_channel_bindings',
    setBinding: 'set_channel_binding',
  },
  rules: {
    list: 'list_rules',
    setEnabled: 'set_rule_enabled',
    edit: 'edit_rule',
    evidence: 'rule_evidence',
  },
  keywords: {
    list: 'list_keywords',
    pendingAliases: 'list_pending_aliases',
    resolveAlias: 'resolve_alias',
  },
  style: {
    getStats: 'get_style_stats',
  },
  evaluation: {
    pickDir: 'pick_eval_dir',
    listFiles: 'list_eval_files',
    readResult: 'read_eval_result',
  },
  diagnostics: {
    checkUpdate: 'check_update',
    getVersion: 'get_version',
    exportDiagnostics: 'export_diagnostics',
  },
  attachment: {
    readImage: 'read_attachment_image',
    clearCache: 'clear_image_cache',
  },
  system: {
    saveFileDialog: 'save_file_dialog',
    openFileDialog: 'open_file_dialog',
    openDirectoryDialog: 'open_directory_dialog',
    confirmCloseAction: 'confirm_close_action',
    getImageSourceDir: 'get_image_source_dir',
    setImageSourceDir: 'set_image_source_dir',
  },
} as const

type DomainMap = typeof COMMANDS

/** 命令域（chat / session / …） */
export type CommandDomain = keyof DomainMap

/** 全部命令名（Rust 侧 snake_case） */
export type CommandName = {
  [Domain in CommandDomain]: DomainMap[Domain][keyof DomainMap[Domain]]
}[CommandDomain]

/** 扁平化命令名列表（按域顺序），供校验与调试使用 */
export function commandNames(): CommandName[] {
  return Object.values(COMMANDS).flatMap(
    (domain) => Object.values(domain) as CommandName[],
  )
}
