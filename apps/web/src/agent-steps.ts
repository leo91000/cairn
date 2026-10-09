import type { ActivityArtifact } from './activity'

/** One line of the agent's timeline; consecutive reads fold into a single step. Mirrors Android's AgentSteps.kt. */
export interface AgentStep {
  id: string
  items: ActivityArtifact[]
  title: string
  detail: string
  failed: boolean
  running: boolean
}

const fileName = (path: string) => path.split('/').at(-1) ?? path
const isRunning = (artifact: ActivityArtifact) => artifact.status === 'running'

export function agentSteps(artifacts: ActivityArtifact[]): AgentStep[] {
  const steps: AgentStep[] = []
  const step = (items: ActivityArtifact[], title: string, detail: string): AgentStep => ({
    id: items[0]!.id,
    items,
    title,
    detail,
    failed: items.some(item => item.status === 'error'),
    running: items.some(isRunning),
  })
  let index = 0
  while (index < artifacts.length) {
    const artifact = artifacts[index]!
    const settledRead = (item?: ActivityArtifact) => item?.kind === 'read' && item.status !== 'error' && !isRunning(item)
    if (settledRead(artifact)) {
      let end = index
      while (settledRead(artifacts[end + 1]))
        end++
      const group = artifacts.slice(index, end + 1)
      const files = [...new Set(group.flatMap(item => item.files.map(file => fileName(file.path))))]
      steps.push(step(group, group.length > 1 ? `Read ${files.length} files` : artifact.title, files.join(' · ')))
      index = end + 1
      continue
    }

    const firstLine = () => artifact.blocks.map(block => block.code).join('\n').split('\n').find(line => line.trim()) ?? ''
    const detail = artifact.kind === 'files'
      ? artifact.files.map(file => fileName(file.path)).join(' · ') || artifact.subtitle
      : artifact.kind === 'thinking'
        ? firstLine()
        // A session notice without a subtitle carries its message in its text, as in ChatNotice.
        : artifact.kind === 'notice' ? artifact.subtitle || firstLine() : artifact.command || artifact.subtitle
    steps.push(step([artifact], artifact.title, detail))
    index++
  }

  return steps
}

/** "Read 2 files, searched once, edited 2 files, ran 1 command". */
export function actionSentence(artifacts: ActivityArtifact[]): string {
  const count = (value: number, one: string, many: string) => value === 1 ? one : many.replace('#', value.toLocaleString())
  const parts: string[] = []
  const reads = artifacts.filter(item => item.kind === 'read').reduce((total, item) => total + Math.max(1, item.files.length), 0)
  if (reads)
    parts.push(count(reads, 'read 1 file', 'read # files'))
  const searches = artifacts.filter(item => item.kind === 'search' || item.kind === 'browse').length
  if (searches)
    parts.push(count(searches, 'searched once', 'searched # times'))
  const edits = artifacts.filter(item => item.kind === 'files').reduce((total, item) => total + Math.max(1, item.files.length), 0)
  if (edits)
    parts.push(count(edits, 'edited 1 file', 'edited # files'))
  const commands = artifacts.filter(item => item.kind === 'command' || item.kind === 'output').length
  if (commands)
    parts.push(count(commands, 'ran 1 command', 'ran # commands'))
  const tools = artifacts.filter(item => item.kind === 'tool').length
  if (tools)
    parts.push(count(tools, 'used 1 tool', 'used # tools'))
  if (artifacts.some(item => item.kind === 'plan'))
    parts.push('updated the plan')
  if (parts.length) {
    const sentence = parts.join(', ')
    return sentence[0]!.toUpperCase() + sentence.slice(1)
  }

  return artifacts.some(item => item.kind === 'thinking') ? 'Thought it through' : 'Followed the run'
}
