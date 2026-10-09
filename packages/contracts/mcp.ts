import type { Tool } from '@modelcontextprotocol/client'
import { z } from 'zod'

export const mcpInput = z.object({
  name: z.string().trim().min(1).max(100),
  transport: z.enum(['http', 'stdio']).default('http'),
  url: z.string().max(2000).default(''),
  command: z.string().trim().max(1000).default(''),
  args: z.array(z.string().max(4000)).max(100).default([]),
  auth: z.enum(['none', 'bearer', 'oauth']).default('none'),
  clientId: z.string().trim().max(500).default(''),
  scopes: z.string().trim().max(2000).default(''),
  allowPrivateNetwork: z.boolean().default(false),
  enabled: z.boolean().default(true),
  enabledTools: z.array(z.string().min(1).max(200)).max(500).nullable().default(null),
  token: z.string().max(10000).optional(),
  clientSecret: z.string().max(10000).optional(),
  removeEnv: z.array(z.string()).max(100).optional(),
  env: z.record(z.string().regex(/^[A-Z_]\w*$/i), z.string().max(10000)).optional(),
}).superRefine((value, ctx) => {
  if (value.transport === 'http') {
    try {
      const url = new URL(value.url)
      if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || url.hash)
        throw new Error('Invalid MCP URL')
    }
    catch {
      ctx.addIssue({ code: 'custom', path: ['url'], message: 'Enter an HTTP(S) URL without embedded credentials or a fragment.' })
    }
  }

  if (value.transport === 'stdio' && !value.command)
    ctx.addIssue({ code: 'custom', path: ['command'], message: 'Enter an executable command.' })
  if (value.transport === 'stdio' && value.auth !== 'none')
    ctx.addIssue({ code: 'custom', path: ['auth'], message: 'Command servers use environment variables for authentication.' })
})
export type McpInput = z.infer<typeof mcpInput>
export type McpTool = Tool
export interface McpConnection extends Omit<McpInput, 'token' | 'clientSecret' | 'env' | 'removeEnv'> {
  id: string
  createdAt: number
  revision: number
  state: 'untested' | 'connected' | 'needs-auth' | 'error'
  error: string
  tools: McpTool[]
  checkedAt: number | null
}
export interface McpView extends McpConnection {
  hasToken: boolean
  hasClientSecret: boolean
  envKeys: string[]
  callbackUrl: string
}
