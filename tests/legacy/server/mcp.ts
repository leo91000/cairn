import type { FastifyInstance } from 'fastify'
import type { Auth } from './auth.ts'
import type { Service } from './service.ts'
import type { Worker } from './worker.ts'
import { toNodeHandler } from '@modelcontextprotocol/node'
import { createMcpHandler, McpServer } from '@modelcontextprotocol/server'
import { z } from 'zod'
import {
  agentInput,
  agentUpdate,
  projectInput,
  taskInput,
} from '../../../shared/contracts.ts'
import { mcpInput } from '../../../shared/mcp.ts'
import { version } from '../../../shared/version.ts'
import { AppError, requireValue } from './errors.ts'

export function mountMcp(
  app: FastifyInstance,
  service: Service,
  worker: Worker,
  auth: Auth,
) {
  app.all('/mcp', async (request, reply) => {
    const bearer = request.headers.authorization?.match(/^Bearer (.+)$/i)?.[1]
    try {
      auth.verify(bearer)
    }
    catch {
      reply.header(
        'WWW-Authenticate',
        `Bearer resource_metadata="${service.config.publicUrl}/.well-known/oauth-protected-resource/mcp"`,
      )
      return reply.code(401).send({ error: 'unauthorized' })
    }

    const handler = createMcpHandler(
      () => {
        const server = new McpServer({
          name: 'leo-agent-manager',
          version,
        })
        const tool = <Shape extends z.ZodRawShape>(
          name: string,
          description: string,
          schema: z.ZodObject<Shape>,
          scope: string,
          fn: (args: z.output<z.ZodObject<Shape>>) => unknown,
          destructive = false,
        ) => {
          server.registerTool(
            name,
            {
              description,
              title: name.replaceAll('_', ' '),
              inputSchema: schema,
              outputSchema: z.object({ result: z.unknown() }),
              _meta: { securitySchemes: [{ type: 'oauth2', scopes: [scope] }] },
              annotations: {
                readOnlyHint: scope === 'read',
                destructiveHint:
                  destructive
                  || scope === 'run'
                  || name.startsWith('update_')
                  || name === 'save_skill',
                openWorldHint: scope !== 'read',
              },
            },
            async (args) => {
              try {
                auth.verify(bearer, scope)
                const result = await fn(schema.parse(args))
                return {
                  structuredContent: { result },
                  content: [
                    { type: 'text' as const, text: JSON.stringify(result) },
                  ],
                }
              }
              catch (e) {
                return {
                  isError: true,
                  ...(e instanceof AppError && [401, 403].includes(e.statusCode)
                    ? {
                        _meta: {
                          'mcp/www_authenticate': `Bearer error="insufficient_scope", error_description="The ${scope} scope is required", scope="${scope}", resource_metadata="${service.config.publicUrl}/.well-known/oauth-protected-resource/mcp"`,
                        },
                      }
                    : {}),
                  content: [
                    { type: 'text' as const, text: (e as Error).message },
                  ],
                }
              }
            },
          )
        }

        tool(
          'list_agents',
          'List configured Codex agent profiles.',
          z.object({}),
          'read',
          () => service.store.list('agents'),
        )
        tool(
          'save_agent',
          'Create an agent profile. Execution access is explicit.',
          agentInput,
          'manage',
          args => service.agent(args),
        )
        tool(
          'update_agent',
          'Update an existing agent, including MCP connection and tool access. Omitted fields retain their current values; access, when supplied, replaces the entire access policy. Changes apply to new runs.',
          z.object({ id: z.string().uuid(), agent: agentUpdate }),
          'manage',
          args => service.agent({ ...requireValue(service.store.get('agents', args.id)), ...args.agent }, args.id),
        )
        tool(
          'list_mcps',
          'List MCP connections, discovered tools, authentication state and saved credential indicators. Secret values are never returned.',
          z.object({}),
          'read',
          () => service.mcps.list(),
        )
        tool(
          'create_mcp',
          'Add an HTTP or command MCP connection. Supports bearer tokens, OAuth and command environment credentials, stored encrypted and never returned. Does not connect or execute commands. For OAuth, open managementUrl and choose Connect in the signed-in browser. Use test_mcp to discover tools, then update_agent to grant access.',
          mcpInput,
          'manage',
          async args => ({ ...await service.mcps.save(args), managementUrl: `${service.config.publicUrl}/mcps` }),
        )
        tool(
          'update_mcp',
          'Replace an existing MCP connection configuration; supply its complete non-secret settings. Omit token, clientSecret and env values to preserve saved credentials; removeEnv deletes selected variables. Changing authentication settings clears previous credentials. Invalidates active remote grants. OAuth sign-in is completed from managementUrl in the browser.',
          z.object({ id: z.string().uuid(), connection: mcpInput }),
          'manage',
          async (args) => {
            service.mcps.assertManagementAvailable(args.id)
            return { ...await service.mcps.save(args.connection, args.id), managementUrl: `${service.config.publicUrl}/mcps` }
          },
        )
        tool(
          'test_mcp',
          'Connect to an MCP server and discover its tools. Command servers execute in the manager environment; only test trusted commands. OAuth requiring user consent must first be connected in the MCPs UI. Returns connection state and safe diagnostics.',
          z.object({ id: z.string().uuid() }),
          'manage',
          (args) => {
            service.mcps.assertManagementAvailable(args.id)
            return service.mcps.test(args.id)
          },
          true,
        )
        tool(
          'disconnect_mcp',
          'Remove locally stored MCP credentials and invalidate active remote grants while keeping the connection configuration. Provider-side consent is not revoked; running command processes retain their environment until they stop.',
          z.object({ id: z.string().uuid() }),
          'manage',
          async (args) => {
            service.mcps.assertManagementAvailable(args.id)
            await service.mcps.disconnect(args.id)
            return { disconnected: true }
          },
          true,
        )
        tool(
          'delete_mcp',
          'Delete an MCP connection and its credentials, remove agent selections, and invalidate active remote grants. Running command processes are not stopped.',
          z.object({ id: z.string().uuid() }),
          'manage',
          async (args) => {
            service.mcps.assertManagementAvailable(args.id)
            await service.mcps.disconnect(args.id, true)
            return { deleted: true }
          },
          true,
        )
        tool(
          'list_projects',
          'List registered project workspaces.',
          z.object({}),
          'read',
          () => service.store.list('projects'),
        )
        tool(
          'save_project',
          'Register an existing directory inside an allowed workspace root.',
          projectInput,
          'manage',
          args => service.project(args),
        )
        tool(
          'list_tasks',
          'List task instructions and schedules.',
          z.object({}),
          'read',
          () => service.store.list('tasks'),
        )
        tool(
          'create_task',
          'Create a one-off or scheduled task. This does not execute it immediately.',
          taskInput,
          'manage',
          args => service.task(args),
        )
        tool(
          'update_task',
          'Update an existing task, including pausing its schedule.',
          z.object({ id: z.string().uuid(), task: taskInput }),
          'manage',
          args => service.task(args.task, args.id),
        )
        tool(
          'run_task',
          'Queue a task using its agent’s resource access and execution mode. YOLO is the default inside a private agent VM. Unattended runs never prompt for approval. May modify allowed code and external services as instructed.',
          z.object({ taskId: z.string().uuid() }),
          'run',
          args => service.enqueue(args.taskId, 'mcp'),
        )
        tool(
          'cancel_run',
          'Cancel queued or running work. Already completed external effects are preserved.',
          z.object({ runId: z.string().uuid() }),
          'run',
          (args) => {
            worker.cancel(args.runId)
            return { cancelled: true }
          },
          true,
        )
        tool(
          'list_runs',
          'List recent runs with pagination, without bulky prompts or logs.',
          z.object({
            limit: z.number().int().min(1).max(100).default(20),
            offset: z.number().int().min(0).default(0),
            status: z.string().optional(),
          }),
          'read',
          args => service.store.listRuns(args),
        )
        tool(
          'get_run',
          'Get a run summary and an incremental page of its event log.',
          z.object({
            runId: z.string().uuid(),
            after: z.number().int().min(0).default(0),
          }),
          'read',
          args => ({
            run: requireValue(service.store.run(args.runId)),
            events: service.store.events(args.runId, args.after),
          }),
        )
        tool(
          'list_skills',
          'List global .agents skills or the skills for one registered project.',
          z.object({ projectId: z.string().uuid().optional() }),
          'read',
          args =>
            service.skills.list(
              args.projectId ?? 'global',
              args.projectId
                ? requireValue(service.store.get('projects', args.projectId))
                  .path
                : undefined,
            ),
        )
        tool(
          'save_skill',
          'Create or update SKILL.md with validated frontmatter in .agents/skills.',
          z.object({
            name: z.string(),
            content: z.string().max(100000),
            projectId: z.string().uuid().optional(),
          }),
          'manage',
          args =>
            service.skills.save(
              args.name,
              args.content,
              args.projectId
                ? requireValue(service.store.get('projects', args.projectId))
                  .path
                : undefined,
            ),
        )
        return server
      },
      { legacy: 'stateless' },
    )
    reply.hijack()
    try {
      await toNodeHandler(handler)(request.raw, reply.raw, request.body)
    }
    finally {
      await handler.close()
    }
  })
}
