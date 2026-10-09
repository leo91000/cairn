import type { RunEvent } from '../../../packages/contracts/contracts'
import { describe, expect, it } from 'vitest'
import { activityEntries } from '../src/activity'
import { actionSentence, agentSteps } from '../src/agent-steps'

let id = 0

function command(command: string, exit: number | null, type = 'item.completed'): RunEvent {
  return {
    id: ++id,
    runId: 'chat',
    createdAt: id,
    type,
    text: '',
    payload: {
      item: {
        id: `c${id}`,
        type: 'command_execution',
        command,
        aggregated_output: '',
        exit_code: exit,
        status: exit === null ? 'in_progress' : exit ? 'failed' : 'completed',
      },
    },
  }
}

function edit(...paths: string[]): RunEvent {
  return {
    id: ++id,
    runId: 'chat',
    createdAt: id,
    type: 'item.completed',
    text: '',
    payload: {
      item: {
        id: `f${id}`,
        type: 'file_change',
        status: 'completed',
        changes: paths.map(path => ({ path, kind: 'update' })),
      },
    },
  }
}

function artifacts(events: RunEvent[], chat = true) {
  return activityEntries(events, chat).flatMap(entry => entry.kind === 'group' ? entry.artifacts : [])
}

describe('agent steps', () => {
  it('summarises the actions between two messages in one sentence', () => {
    const items = artifacts([command('sed -n 1,80p src/cart.ts', 0), command('sed -n 1,40p src/cart.test.ts', 0), command('rg -n discount src', 0), edit('src/cart.ts', 'src/cart.test.ts'), command('pnpm test', 1), command('pnpm test', 0)])
    expect(actionSentence(items)).toBe('Read 2 files, searched once, edited 2 files, ran 2 commands')
    expect(actionSentence([])).toBe('Followed the run')
  })

  it('folds consecutive reads and keeps failures and running steps apart', () => {
    const steps = agentSteps(artifacts([command('sed -n 1,80p src/cart.ts', 0), command('cat src/cart.test.ts', 0), edit('src/cart.ts'), command('pnpm test', 1), command('cat README.md', null, 'item.started')]))
    expect(steps.map(({
      title,
      detail,
      failed,
      running,
    }) => ({
      title,
      detail,
      failed,
      running,
    }))).toEqual([
      {
        title: 'Read 2 files',
        detail: 'cart.ts · cart.test.ts',
        failed: false,
        running: false,
      },
      {
        title: 'File changes',
        detail: 'cart.ts',
        failed: false,
        running: false,
      },
      {
        title: 'Run tests',
        detail: 'pnpm test',
        failed: true,
        running: false,
      },
      {
        title: 'Read README.md',
        detail: 'cat README.md',
        failed: false,
        running: true,
      },
    ])
  })

  it('names a failed session notice of a run by its message', () => {
    const [step] = agentSteps(artifacts([{
      id: ++id,
      runId: 'run',
      createdAt: id,
      type: 'error',
      text: 'Validation needs attention.',
      payload: { message: 'Validation needs attention.' },
    }], false))
    expect(step).toMatchObject({ detail: 'Validation needs attention.', failed: true })
  })
})
