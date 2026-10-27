/**
 * tests/mcpSnippet.test.ts — MCP 客户端配置片段
 *
 * 覆盖:
 * - 片段把当前数据库路径经全局 `--db` 传入，并置于子命令之前
 * - 库路径缺失时退化为最小片段，不产出空的 `--db`
 * - 面板信息未就绪时返回空串
 */

import { describe, expect, it } from 'vitest'

import { buildMcpSnippet } from '../src/views/settings/mcpSnippet'

interface Snippet {
  mcpServers: {
    ramaria: {
      command: string
      args: string[]
    }
  }
}

function parse(text: string): Snippet {
  return JSON.parse(text) as Snippet
}

describe('MCP 客户端配置片段', () => {
  it('带库路径时 --db 置于子命令之前', () => {
    const snippet = parse(
      buildMcpSnippet({
        command: 'C:\\Program Files\\Ramaria\\ramaria.exe',
        dbPath: 'C:\\Users\\demo\\AppData\\Roaming\\Ramaria\\data\\ramaria.db',
      }),
    )

    expect(snippet.mcpServers.ramaria.command).toBe('C:\\Program Files\\Ramaria\\ramaria.exe')
    expect(snippet.mcpServers.ramaria.args).toEqual([
      '--db',
      'C:\\Users\\demo\\AppData\\Roaming\\Ramaria\\data\\ramaria.db',
      'mcp',
      'serve',
    ])
  })

  it('库路径为空时退化为最小片段', () => {
    const snippet = parse(buildMcpSnippet({ command: 'ramaria', dbPath: '   ' }))

    expect(snippet.mcpServers.ramaria.args).toEqual(['mcp', 'serve'])
  })

  it('面板信息未就绪时返回空串', () => {
    expect(buildMcpSnippet(null)).toBe('')
  })
})
