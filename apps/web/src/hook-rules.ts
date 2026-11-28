import type { HookRuleBlockKind } from '../../../packages/contracts/generated/typescript/HookRuleBlockKind'
import type { HookRuleCatalog } from '../../../packages/contracts/generated/typescript/HookRuleCatalog'
import type { HookRuleDefinition } from '../../../packages/contracts/generated/typescript/HookRuleDefinition'
import type { HookRuleProblem } from '../../../packages/contracts/generated/typescript/HookRuleProblem'
import type { HookRuleTrigger } from '../../../packages/contracts/generated/typescript/HookRuleTrigger'
import type { HookRuleTriggeringStatus } from '../../../packages/contracts/generated/typescript/HookRuleTriggeringStatus'
import type { RuleAction } from '../../../packages/contracts/generated/typescript/RuleAction'
import type { RuleCatalogHook } from '../../../packages/contracts/generated/typescript/RuleCatalogHook'
import type { RuleCatalogTool } from '../../../packages/contracts/generated/typescript/RuleCatalogTool'
import type { RuleCondition } from '../../../packages/contracts/generated/typescript/RuleCondition'
import type { RuleNode } from '../../../packages/contracts/generated/typescript/RuleNode'
import type { RuleOperator } from '../../../packages/contracts/generated/typescript/RuleOperator'
import type { RuleValue } from '../../../packages/contracts/generated/typescript/RuleValue'
import type { IconName } from './icons'
import {
  Bookmark,
  Bot,
  Hourglass,
  MessageCircleReply,
  MessageSquareShare,
  OctagonX,
  Repeat,
  Split,
  Wrench,
} from './icons'
import { valueJsonDrafts } from './rule-json-drafts'
import { resolveRuleSchema, ruleLiteralDefault, ruleToolInputSchema } from './rule-schema'

// Hook rules (ADR-0075): presentation and editing helpers shared by the list, the detail
// and the visual editor. The installation validates and authorizes everything again.

export type ActionKind = RuleAction['type']
export type StepKind = ActionKind | 'if' | 'forEach'

export function initialToolInput(root: unknown): Record<string, RuleValue> {
  const schema = ruleToolInputSchema(root)
  const required: string[] = Array.isArray(schema.required) ? schema.required : []
  return Object.fromEntries(required.map(name => [name, {
    type: 'literal',
    value: ruleLiteralDefault(schema.properties?.[name] ?? {}, root),
  }]))
}

export interface StepChoice {
  kind: StepKind
  title: string
  description: string
  icon: IconName
}

export const stepChoices: StepChoice[] = [
  {
    kind: 'callTool',
    title: 'Call a plugin tool',
    description: 'Post, react, read… with the execution agent’s rights.',
    icon: Wrench,
  },
  {
    kind: 'startAgent',
    title: 'Start an agent',
    description: 'Open a conversation, optionally bound to a key.',
    icon: Bot,
  },
  {
    kind: 'steerConversation',
    title: 'Steer a conversation',
    description: 'Send a message to the conversation of a key.',
    icon: MessageSquareShare,
  },
  {
    kind: 'answerQuestion',
    title: 'Answer a question',
    description: 'Reply to the agent’s pending question.',
    icon: MessageCircleReply,
  },
  {
    kind: 'cancelConversation',
    title: 'Stop a conversation',
    description: 'Stop the current turn of a conversation.',
    icon: OctagonX,
  },
  {
    kind: 'storeValue',
    title: 'Remember a value',
    description: 'Keep a value for the next triggerings.',
    icon: Bookmark,
  },
  {
    kind: 'wait',
    title: 'Wait',
    description: 'Pause, then continue (up to 7 days).',
    icon: Hourglass,
  },
  {
    kind: 'if',
    title: 'If / otherwise',
    description: 'Choose between two paths.',
    icon: Split,
  },
  {
    kind: 'forEach',
    title: 'For each',
    description: 'Repeat steps for every item of a list.',
    icon: Repeat,
  },
]

export function stepChoice(kind: StepKind) {
  return stepChoices.find(choice => choice.kind === kind)!
}

export const cairnHookTitles: Record<string, string> = {
  replyReady: 'Reply ready',
  questionAsked: 'Question asked',
  progress: 'Progress',
  ended: 'Turn ended',
}

export const blockTitles: Record<HookRuleBlockKind, string> = {
  disabled: 'Disabled',
  emergencyPaused: 'Paused by the owner',
  budgetExhausted: 'Hourly limit reached',
  rightsLost: 'Rights lost',
  membershipEnded: 'Creator left',
}

export const blockLifters: Record<HookRuleBlockKind, string> = {
  disabled: 'Its creator can enable it.',
  emergencyPaused: 'Only the installation owner can resume it.',
  budgetExhausted: 'Its creator or the owner can resume it.',
  rightsLost: 'It resumes by itself once the right returns, or when its creator edits it.',
  membershipEnded: 'It never resumes: duplicate it to use it again.',
}

export const statusTitles: Record<HookRuleTriggeringStatus, string> = {
  running: 'Running',
  waiting: 'Waiting',
  retrying: 'Retrying',
  completed: 'Completed',
  failed: 'Failed',
  stopped: 'Stopped',
}

export function statusTone(status: HookRuleTriggeringStatus) {
  if (status === 'completed')
    return 'success'
  if (status === 'failed')
    return 'danger'
  if (status === 'stopped')
    return 'muted'
  return 'active'
}

export const operatorTitles: Record<RuleOperator, string> = {
  equals: 'is',
  notEquals: 'is not',
  contains: 'contains',
  startsWith: 'starts with',
  endsWith: 'ends with',
  matches: 'matches',
  in: 'is one of',
  present: 'is present',
  absent: 'is absent',
  greaterThan: 'is greater than',
  lessThan: 'is less than',
}

export const binaryOperator = (operator: RuleOperator) => operator !== 'present' && operator !== 'absent'

export function triggerKey(trigger: HookRuleTrigger) {
  return trigger.type === 'plugin' ? `plugin:${trigger.instanceId}:${trigger.hook}` : `cairn:${trigger.hook}`
}

export function catalogHook(catalog: HookRuleCatalog | null, trigger: HookRuleTrigger): RuleCatalogHook | undefined {
  return catalog?.hooks.find(hook => triggerKey(hook.trigger) === triggerKey(trigger))
}

export function catalogTool(catalog: HookRuleCatalog | null, instanceId: string, tool: string): RuleCatalogTool | undefined {
  return catalog?.tools.find(entry => entry.instanceId === instanceId && entry.tool === tool)
}

export function describeTrigger(trigger: HookRuleTrigger, catalog: HookRuleCatalog | null) {
  const hook = catalogHook(catalog, trigger)
  if (trigger.type === 'cairn') {
    return { title: cairnHookTitles[trigger.hook] ?? trigger.hook, source: 'Cairn', description: hook?.description ?? '' }
  }

  return {
    title: hook?.title ?? trigger.hook,
    source: hook?.instanceName ?? 'Plugin instance',
    description: hook?.description ?? '',
  }
}

/** A value as one short line: fixed text, text with insertions, or a formula. */
export function describeValue(value: RuleValue | null | undefined): string {
  if (!value)
    return ''
  if (value.type === 'literal')
    return typeof value.value === 'string' ? value.value : JSON.stringify(value.value)
  if (value.type === 'template')
    return value.text
  return `ƒ ${value.expression}`
}

function quoted(value: RuleValue | null | undefined) {
  const text = describeValue(value)
  return value?.type === 'literal' && typeof value.value === 'string' ? `“${text}”` : text
}

export function describeCondition(condition: RuleCondition): string {
  switch (condition.type) {
    case 'all':
    case 'any': {
      const parts = condition.conditions.map(describeCondition).filter(Boolean)
      if (parts.length === 0)
        return 'Add a condition'
      return parts.length === 1 ? parts[0] : parts.map(part => part.includes(' and ') || part.includes(' or ') ? `(${part})` : part).join(condition.type === 'all' ? ' and ' : ' or ')
    }

    case 'not':
      return `not (${describeCondition(condition.condition)})`
    case 'compare':
      return [describeValue(condition.left), operatorTitles[condition.operator], binaryOperator(condition.operator) ? quoted(condition.right) : ''].filter(Boolean).join(' ')
    case 'formula':
      return `ƒ ${condition.expression}`
    case 'authorIn':
      return `${describeValue(condition.author)} is one of ${condition.people.length} ${condition.people.length === 1 ? 'person' : 'people'}`
    case 'authorStartedConversation':
      return `${describeValue(condition.author)} started ${quoted(condition.key)}`
    case 'conversationExists':
      return `a conversation exists for ${quoted(condition.key)}`
    case 'authorQuota':
      return `${describeValue(condition.author)} triggered fewer than ${condition.limit} times in ${duration(condition.windowMinutes * 60)}`
  }
}

export function duration(seconds: number) {
  if (seconds % 86400 === 0)
    return `${seconds / 86400} ${seconds === 86400 ? 'day' : 'days'}`
  if (seconds % 3600 === 0)
    return `${seconds / 3600} ${seconds === 3600 ? 'hour' : 'hours'}`
  if (seconds % 60 === 0)
    return `${seconds / 60} min`
  return `${seconds} s`
}

export interface ActionSummary {
  title: string
  detail: string
  icon: IconName
}

export function describeAction(action: RuleAction, catalog: HookRuleCatalog | null, agentName: (id: string) => string): ActionSummary {
  const icon = stepChoice(action.type).icon
  switch (action.type) {
    case 'callTool': {
      const tool = catalogTool(catalog, action.instanceId, action.tool)
      const fields = Object.entries(action.input).slice(0, 2).map(([key, value]) => `${key}: ${describeValue(value)}`)
      return { icon, title: tool ? `${tool.instanceName} · ${tool.description || tool.tool}` : action.tool, detail: fields.join(' · ') }
    }

    case 'startAgent':
      return { icon, title: `Start ${agentName(action.agentId)}`, detail: [describeValue(action.prompt), action.author ? `author ${describeValue(action.author)}` : '', action.key ? `key ${describeValue(action.key)}` : ''].filter(Boolean).join(' · ') }
    case 'steerConversation':
      return { icon, title: 'Steer the conversation', detail: `${describeValue(action.key)} · ${describeValue(action.message)}` }
    case 'answerQuestion':
      return { icon, title: 'Answer its question', detail: `${describeValue(action.key)} · ${describeValue(action.answer)}` }
    case 'cancelConversation':
      return { icon, title: 'Stop the conversation', detail: describeValue(action.key) }
    case 'storeValue':
      return { icon, title: `Remember ${action.name}`, detail: `${action.scope === 'conversation' ? `on ${describeValue(action.key)} · ` : ''}${describeValue(action.value)}` }
    case 'wait': {
      const seconds = action.seconds.type === 'literal' && typeof action.seconds.value === 'number' ? duration(action.seconds.value) : describeValue(action.seconds)
      return { icon, title: `Wait ${seconds}`, detail: 'Then continue with the next steps.' }
    }
  }
}

// ——— Tree editing. Paths are JSON Pointers into the definition, as validation reports them.

export type Container = '/steps' | `${string}/then` | `${string}/otherwise` | `${string}/body`

export function nodesAt(definition: HookRuleDefinition, container: string): RuleNode[] {
  const parts = container.split('/').filter(Boolean)
  let current: any = definition
  for (const part of parts)
    current = current?.[/^\d+$/.test(part) ? Number(part) : part]
  return Array.isArray(current) ? current : []
}

export function nodeAt(definition: HookRuleDefinition, path: string): RuleNode | undefined {
  const index = path.lastIndexOf('/')
  return nodesAt(definition, path.slice(0, index))[Number(path.slice(index + 1))]
}

export function allNodes(nodes: RuleNode[]): RuleNode[] {
  return nodes.flatMap(node => [node, ...(node.type === 'if' ? [...allNodes(node.then), ...allNodes(node.otherwise)] : node.type === 'forEach' ? allNodes(node.body) : [])])
}

export function uniqueId(definition: HookRuleDefinition, base: string) {
  const taken = new Set(allNodes(definition.steps).map(node => node.id))
  const stem = base.replace(/\W/g, '').replace(/^[^a-z]+/, '') || 'step'
  if (!taken.has(stem))
    return stem
  for (let index = 2; ; index++) {
    if (!taken.has(`${stem}${index}`))
      return `${stem}${index}`
  }
}

function uniqueItemName(definition: HookRuleDefinition): string {
  const taken = new Set(allNodes(definition.steps).filter(node => node.type === 'forEach').map(node => node.itemName))
  let name = 'item'
  for (let index = 2; taken.has(name); index++)
    name = `item${index}`
  return name
}

const idBases: Record<StepKind, string> = {
  callTool: 'call',
  startAgent: 'start',
  steerConversation: 'steer',
  answerQuestion: 'answer',
  cancelConversation: 'stop',
  storeValue: 'remember',
  wait: 'wait',
  if: 'check',
  forEach: 'each',
}

export function emptyValue(text = ''): RuleValue {
  return { type: 'template', text }
}

export function optionalRuleText(value: RuleValue): RuleValue | null {
  if (value.type === 'template' && !value.text)
    return null
  if (value.type === 'literal' && (value.value === '' || value.value === null))
    return null
  return value
}

export function newStep(definition: HookRuleDefinition, kind: StepKind, catalog: HookRuleCatalog | null): RuleNode {
  const id = uniqueId(definition, idBases[kind])
  const agent = definition.executionAgentId
  switch (kind) {
    case 'if':
      return {
        type: 'if',
        id,
        condition: { type: 'all', conditions: [] },
        then: [],
        otherwise: [],
      }
    case 'forEach':
      return {
        type: 'forEach',
        id,
        items: { type: 'formula', expression: 'payload.' },
        itemName: uniqueItemName(definition),
        body: [],
      }
    case 'callTool': {
      const tool = catalog?.tools.find(entry => entry.agentIds.includes(agent)) ?? catalog?.tools[0]
      return {
        type: 'action',
        id,
        action: {
          type: 'callTool',
          instanceId: tool?.instanceId ?? '',
          tool: tool?.tool ?? '',
          input: initialToolInput(tool?.inputSchema),
        },
      }
    }

    case 'startAgent':
      return {
        type: 'action',
        id,
        action: {
          type: 'startAgent',
          agentId: agent,
          prompt: emptyValue(),
          key: null,
          projectId: null,
        },
      }
    case 'steerConversation':
      return { type: 'action', id, action: { type: 'steerConversation', key: emptyValue(), message: emptyValue() } }
    case 'answerQuestion':
      return { type: 'action', id, action: { type: 'answerQuestion', key: emptyValue(), answer: emptyValue() } }
    case 'cancelConversation':
      return { type: 'action', id, action: { type: 'cancelConversation', key: emptyValue() } }
    case 'storeValue':
      return {
        type: 'action',
        id,
        action: {
          type: 'storeValue',
          scope: 'rule',
          key: null,
          name: 'last',
          value: emptyValue(),
        },
      }
    case 'wait':
      return { type: 'action', id, action: { type: 'wait', seconds: { type: 'literal', value: 600 } } }
  }
}

function clone<T>(value: T): T {
  // Rule definitions are wire JSON; this also unwraps Vue's nested proxies.
  return JSON.parse(JSON.stringify(value))
}

export function pathOfNode(definition: HookRuleDefinition, id: string): string | null {
  function search(nodes: RuleNode[], container: string): string | null {
    for (const [index, node] of nodes.entries()) {
      const path = `${container}/${index}`
      if (node.id === id)
        return path
      const child = node.type === 'if'
        ? search(node.then, `${path}/then`) ?? search(node.otherwise, `${path}/otherwise`)
        : node.type === 'forEach' ? search(node.body, `${path}/body`) : null
      if (child)
        return child
    }

    return null
  }

  return search(definition.steps, '/steps')
}

export function withInserted(definition: HookRuleDefinition, container: string, index: number, node: RuleNode) {
  const next = clone(definition)
  nodesAt(next, container).splice(index, 0, node)
  return next
}

export function withRemoved(definition: HookRuleDefinition, path: string) {
  const next = clone(definition)
  const slash = path.lastIndexOf('/')
  nodesAt(next, path.slice(0, slash)).splice(Number(path.slice(slash + 1)), 1)
  return next
}

export function withReplaced(definition: HookRuleDefinition, path: string, node: RuleNode) {
  const next = clone(definition)
  const slash = path.lastIndexOf('/')
  nodesAt(next, path.slice(0, slash))[Number(path.slice(slash + 1))] = node
  return next
}

export function withMoved(definition: HookRuleDefinition, path: string, offset: -1 | 1) {
  const next = clone(definition)
  const slash = path.lastIndexOf('/')
  const nodes = nodesAt(next, path.slice(0, slash))
  const from = Number(path.slice(slash + 1))
  const to = from + offset
  if (to < 0 || to >= nodes.length)
    return definition
  const [node] = nodes.splice(from, 1)
  nodes.splice(to, 0, node)
  return next
}

export function emptyDefinition(trigger: HookRuleTrigger, agentId: string, limits: HookRuleDefinition['limits']): HookRuleDefinition {
  return {
    name: '',
    description: '',
    trigger,
    executionAgentId: agentId,
    steps: [],
    limits,
  }
}

/** Problems of one node, its fields and its nested blocks. */
export function problemsUnder(problems: HookRuleProblem[], path: string) {
  return problems.filter(problem => problem.path === path || problem.path.startsWith(`${path}/`))
}

/** Problems of one node's own fields, not of the blocks it holds. */
export function ownProblems(problems: HookRuleProblem[], path: string) {
  return problemsUnder(problems, path).filter(problem => !/^\/(?:then|otherwise|body)\/\d+/.test(problem.path.slice(path.length)))
}

// ——— Variables that values can insert.

export interface VariableOption {
  group: 'Event' | 'Earlier steps' | 'Remembered' | 'Loop' | 'Time' | 'Context' | 'Functions'
  expression: string
  label: string
  type: string
  description: string
  example?: string
  path?: string
  itemType?: string
  schema?: any
  personId?: boolean
}

function typeOf(schema: any): string {
  const type = Array.isArray(schema?.type) ? schema.type.find((entry: string) => entry !== 'null') : schema?.type
  return typeof type === 'string' ? type : 'value'
}

/** The fields of a JSON Schema, as CEL paths, depth-first and at most four levels deep. */
export function schemaFields(schema: any, prefix: string, depth = 0, root = schema, pointer = ''): VariableOption[] {
  const properties = resolveRuleSchema(schema, root)?.properties
  if (!properties || typeof properties !== 'object' || depth > 3)
    return []
  return Object.entries(properties).flatMap(([name, field]: [string, any]) => {
    const resolvedField = resolveRuleSchema(field, root)
    const identifier = /^[a-z_]\w*$/i.test(name)
    const expression = identifier ? `${prefix}.${name}` : `${prefix}[${JSON.stringify(name)}]`
    const path = `${pointer}/${name.replace(/~/g, '~0').replace(/\//g, '~1')}`
    return [
      {
        path,
        itemType: typeOf(resolveRuleSchema(resolvedField?.items, root)),
        schema: resolvedComparisonSchema(resolvedField, root),
        group: 'Event' as const,
        expression,
        label: expression.replace(/^payload\./, ''),
        type: typeOf(resolvedField),
        description: field?.description ?? '',
      },
      ...schemaFields(field, expression, depth + 1, root, path),
    ]
  })
}

function actionsBefore(nodes: RuleNode[], container: string, target: string, found: { done: boolean }): RuleNode[] {
  const before: RuleNode[] = []
  for (const [index, node] of nodes.entries()) {
    const path = `${container}/${index}`
    if (path === target) {
      found.done = true
      return before
    }

    if (node.type === 'if') {
      const then = actionsBefore(node.then, `${path}/then`, target, found)
      if (found.done)
        return [...before, ...then]
      const otherwise = actionsBefore(node.otherwise, `${path}/otherwise`, target, found)
      if (found.done)
        return [...before, ...otherwise]
      before.push(...then, ...otherwise)
    }
    else if (node.type === 'forEach') {
      before.push(...actionsBefore(node.body, `${path}/body`, target, found))
      if (found.done)
        return before
    }
    else {
      before.push(node)
    }
  }

  return before
}

function loopsAround(definition: HookRuleDefinition, path: string) {
  const loops: Array<{ name: string, items: RuleValue }> = []
  const parts = path.split('/').filter(Boolean)
  let nodes: any = definition.steps
  for (let index = 1; index < parts.length; index += 2) {
    const node = nodes?.[Number(parts[index])]
    if (node?.type === 'forEach' && parts[index + 1] === 'body')
      loops.push({ name: node.itemName, items: node.items })
    nodes = node?.[parts[index + 1]]
  }

  return loops.reverse().filter((loop, index, all) => all.findIndex(other => other.name === loop.name) === index)
}

function rememberedNames(nodes: RuleNode[]): string[] {
  return [...new Set(nodes.flatMap(node => node.type === 'if'
    ? [...rememberedNames(node.then), ...rememberedNames(node.otherwise)]
    : node.type === 'forEach'
      ? rememberedNames(node.body)
      : node.action.type === 'storeValue' && node.action.scope === 'rule' && node.action.name ? [node.action.name] : []))]
}

/** What a value at this place of the tree can read. */
export function variablesAt(definition: HookRuleDefinition, path: string, catalog: HookRuleCatalog | null): VariableOption[] {
  const hook = catalogHook(catalog, definition.trigger)
  const fields = schemaFields(hook?.payloadSchema, 'payload').map(field => ({ ...field, personId: hook?.resourceFields.some(resource => resource.path === field.path && (/person|user/i.test(resource.resourceType) || /author|sender/i.test(resource.path))) ?? false }))
  const earlier = actionsBefore(definition.steps, '/steps', path, { done: false })
  return [
    ...fields,
    ...earlier.map(node => ({
      group: 'Earlier steps' as const,
      expression: `steps.${node.id}`,
      label: node.type === 'action' ? node.label || describeAction(node.action, catalog, () => 'agent').title : node.id,
      type: 'result',
      description: node.type === 'action' ? `The result of “${node.id}”.` : '',
    })),
    ...loopsAround(definition, path).map(loop => ({
      group: 'Loop' as const,
      expression: loop.name,
      label: loop.name,
      type: 'item',
      description: `The current item of ${describeValue(loop.items)}.`,
    })),
    ...rememberedNames(definition.steps).map(name => ({
      group: 'Remembered' as const,
      expression: `values[${JSON.stringify(name)}]`,
      label: name,
      type: 'value',
      description: `The value this rule remembers as ${name}.`,
    })),
    ...(catalog?.variables ?? [{ name: 'values', description: 'Remembered values.' }, { name: 'now', description: 'When the triggering started.' }, { name: 'event', description: 'Event metadata.' }]).map(variable => ({
      group: (variable.name === 'values' ? 'Remembered' : variable.name === 'now' ? 'Time' : 'Context') as VariableOption['group'],
      expression: variable.name,
      label: variable.name,
      type: variable.name === 'now' ? 'timestamp' : 'value',
      description: variable.description,
    })),
    ...['id', 'hook', 'instanceId', 'receivedAt'].map(name => ({
      group: 'Context' as const,
      expression: `event.${name}`,
      label: `event.${name}`,
      type: name === 'receivedAt' ? 'timestamp' : 'string',
      description: 'Event metadata.',
    })),
    ...(catalog?.functions ?? []).map(fn => ({
      group: 'Functions' as const,
      expression: fn.example,
      label: fn.signature,
      type: 'ƒ',
      description: fn.description,
      example: fn.example,
    })),
  ]
}

export function authorVariable(variables: VariableOption[]): string {
  const strings = variables.filter(variable => variable.group === 'Event' && variable.type === 'string')
  return strings.find(variable => variable.personId)?.expression
    ?? strings.find(variable => /(?:^|\.)author(?:\.id|Id)$/i.test(variable.expression))?.expression
    ?? strings.find(variable => /(?:^|\.)author$/i.test(variable.expression))?.expression
    ?? (variables.some(variable => variable.type === 'object' && /(?:^|\.)author$/i.test(variable.expression)) ? '' : 'payload.author')
}

export function comparisonExpects(left: RuleValue, operator: RuleOperator, variables: VariableOption[]): 'text' | 'number' | 'boolean' | 'json' {
  if (operator === 'in')
    return 'json'
  if (operator === 'contains') {
    const field = left.type === 'formula' ? variables.find(variable => variable.expression === left.expression || variable.expression === `payload.${left.expression}`) : undefined
    const items = left.type === 'literal' && Array.isArray(left.value) ? left.value : null
    if (items || field?.type === 'array') {
      const itemTypes = items ? [...new Set(items.map(item => typeof item))] : []
      const itemType = items ? itemTypes.length === 1 ? itemTypes[0] : 'value' : field?.itemType
      return itemType === 'integer' || itemType === 'number' ? 'number' : itemType === 'boolean' ? 'boolean' : itemType === 'string' ? 'text' : 'json'
    }

    if (left.type === 'formula' && (!field || ['value', 'result'].includes(field.type)))
      return 'json'
    return 'text'
  }

  if (['matches', 'startsWith', 'endsWith'].includes(operator))
    return 'text'
  const type = left.type === 'literal'
    ? typeof left.value
    : left.type === 'formula'
      ? variables.find(variable => variable.expression === left.expression || variable.expression === `payload.${left.expression}`)?.type
      : 'string'
  if (['equals', 'notEquals'].includes(operator) && (!type || ['object', 'array', 'value', 'result', 'item'].includes(type)))
    return 'json'
  if ((!type || type === 'value' || type === 'result') && ['greaterThan', 'lessThan'].includes(operator))
    return 'number'
  return type === 'integer' || type === 'number' ? 'number' : type === 'boolean' ? 'boolean' : 'text'
}

export function ruleConditionDepth(condition: RuleCondition): number {
  if (condition.type === 'all' || condition.type === 'any')
    return 1 + Math.max(0, ...condition.conditions.map(ruleConditionDepth))
  if (condition.type === 'not')
    return 1 + ruleConditionDepth(condition.condition)
  return 1
}

function resolvedComparisonSchema(input: any, root: any, depth = 0): any {
  if (depth > 8)
    return {}
  const schema = resolveRuleSchema(input, root)
  const resolved = { ...schema }
  if (schema.items)
    resolved.items = resolvedComparisonSchema(schema.items, root, depth + 1)
  if (schema.properties)
    resolved.properties = Object.fromEntries(Object.entries(schema.properties).map(([name, field]) => [name, resolvedComparisonSchema(field, root, depth + 1)]))
  return resolved
}

function literalComparisonSchema(value: unknown, depth = 0): any {
  if (depth > 8 || value === null)
    return {}
  if (Array.isArray(value)) {
    const candidates = value.map(item => literalComparisonSchema(item, depth + 1))
    const items = candidates.length && candidates.every(item => JSON.stringify(item) === JSON.stringify(candidates[0])) ? candidates[0] : {}
    return { type: 'array', items }
  }

  return { type: typeof value === 'object' ? 'object' : typeof value === 'string' ? 'string' : typeof value }
}

export function comparisonListSchema(left: RuleValue, variables: VariableOption[]) {
  if (left.type === 'literal')
    return { type: 'array', items: literalComparisonSchema(left.value) }
  const field = left.type === 'formula' ? variables.find(variable => variable.expression === left.expression || variable.expression === `payload.${left.expression}`) : undefined
  if (field?.schema)
    return { type: 'array', items: field.schema }
  const type = comparisonExpects(left, 'equals', variables)
  return { type: 'array', items: type === 'json' ? {} : { type: type === 'text' ? 'string' : type } }
}

// Only schema-level optional fields are normalized. Literal JSON data, including
// null properties, keeps its exact meaning.
export function sameRuleDefinition(left: HookRuleDefinition, right: HookRuleDefinition): boolean {
  const present = <T extends object>(value: T) => Object.fromEntries(Object.entries(value).filter(([, item]) => item !== null))
  const condition = (value: RuleCondition): object => {
    if (value.type === 'all' || value.type === 'any')
      return { ...present(value), conditions: value.conditions.map(condition) }
    if (value.type === 'not')
      return { ...present(value), condition: condition(value.condition) }
    return present(value)
  }

  const node = (value: RuleNode): object => {
    const fields = present(value)
    if (fields.label === '')
      delete fields.label
    if (value.type === 'if') {
      return {
        ...fields,
        condition: condition(value.condition),
        then: value.then.map(node),
        otherwise: value.otherwise.map(node),
      }
    }

    if (value.type === 'forEach')
      return { ...fields, body: value.body.map(node) }
    return { ...fields, action: present(value.action) }
  }

  const stable = (value: HookRuleDefinition) => JSON.stringify({ ...value, steps: value.steps.map(node) }, (_key, entry) =>
    entry && typeof entry === 'object' && !Array.isArray(entry)
      ? Object.fromEntries(Object.entries(entry).sort(([a], [b]) => a.localeCompare(b)))
      : entry)
  return stable(left) === stable(right)
}

export function typedComparisonRight(value: RuleValue | null | undefined, expects: ReturnType<typeof comparisonExpects>, listOnly = true): RuleValue {
  if (value?.type === 'literal' && Object.keys(valueJsonDrafts(value)).length)
    return value

  if (value?.type === 'formula' || (value?.type === 'template' && expects === 'json' && !listOnly))
    return value
  if (value?.type === 'template') {
    const expression = /^\s*\{\{([^{}]+)\}\}\s*$/.exec(value.text)?.[1]
    if (expression && expects !== 'text')
      return { type: 'formula', expression: expression.trim() }
  }

  if (value?.type === 'literal' && value.value === null && !listOnly)
    return value

  const raw = value?.type === 'literal' ? value.value : value?.text ?? ''
  if (expects === 'number')
    return { type: 'literal', value: Number.isFinite(Number(raw)) ? Number(raw) : 0 }
  if (expects === 'boolean')
    return { type: 'literal', value: raw === true || raw === 'true' }
  if (expects === 'json') {
    if (!listOnly) {
      if (value?.type === 'literal')
        return value
      try {
        return { type: 'literal', value: JSON.parse(String(raw)) }
      }
      catch {
        return { type: 'literal', value: raw }
      }
    }

    if (Array.isArray(raw))
      return { type: 'literal', value: raw }
    try {
      const list = JSON.parse(String(raw))
      if (Array.isArray(list))
        return { type: 'literal', value: list }
    }
    catch {}

    return { type: 'literal', value: [] }
  }

  if (value?.type === 'literal' && typeof value.value !== 'string')
    return { type: 'literal', value: typeof value.value === 'object' ? JSON.stringify(value.value) : String(value.value) }
  return value ?? { type: 'template', text: '' }
}

export function relativeTime(at: number, now = Date.now()) {
  const seconds = Math.round((now - at) / 1000)
  if (seconds < 45)
    return 'just now'
  if (seconds < 3600)
    return `${Math.round(seconds / 60)} min ago`
  if (seconds < 86400)
    return `${Math.round(seconds / 3600)} h ago`
  return new Date(at).toLocaleDateString(undefined, { day: 'numeric', month: 'short' })
}
