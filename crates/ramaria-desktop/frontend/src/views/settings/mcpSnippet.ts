/**
 * src/views/settings/mcpSnippet.ts — MCP 客户端配置片段
 *
 * 职责:
 * - 由 CLI 命令与数据库路径生成通用 `mcpServers` JSON 片段
 *
 * 设计特点:
 * - 纯函数：命令与库路径由调用方注入，不探测路径、不读全局状态
 * - 库路径经全局 `--db` 传入并置于子命令之前：MCP 客户端的启动目录不确定，
 *   缺省会落到相对路径 `data/ramaria.db`，与桌面库不是同一份
 * - 库路径为空时退化为最小片段，不产出空的 `--db`
 */

/** 片段所需的 MCP 面板字段 */
export interface McpSnippetSource {
  command: string
  dbPath: string
}

/**
 * 生成客户端配置片段。
 *
 * 参数:
 * - `source`: CLI 命令与当前数据库绝对路径；`null` 表示信息未就绪
 *
 * 返回:
 * - 缩进后的 JSON 文本；信息未就绪时为空串
 */
export function buildMcpSnippet(source: McpSnippetSource | null): string {
  if (!source) {
    return ''
  }
  const dbPath = source.dbPath.trim()
  const args = dbPath ? ['--db', dbPath, 'mcp', 'serve'] : ['mcp', 'serve']
  const config = {
    mcpServers: {
      ramaria: {
        command: source.command,
        args,
      },
    },
  }
  return JSON.stringify(config, null, 2)
}
