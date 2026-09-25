import type {
  Agent,
  Project,
  Run,
  Task,
} from './contracts.ts'
import { accessPolicy, MAIN_AGENT_ID } from '../../../shared/contracts.ts'
import { AppError } from './errors.ts'

export function policy(agent: Agent) {
  const access = accessPolicy.parse(agent.access ?? {})
  if (agent.id !== MAIN_AGENT_ID && agent.access && !Object.hasOwn(agent.access, 'mcps') && (access.projects !== null || access.skills !== null || !access.github || access.sandbox !== 'yolo'))
    access.mcps = []
  return access
}

export function isolated(agent: Agent) {
  const access = policy(agent)
  return access.projects !== null || access.skills !== null || !access.github || access.sandbox !== 'yolo' || access.mcps !== null || Object.keys(access.mcpTools).length > 0
}

export function allowedProjects(agent: Agent, projects: Project[]) {
  const access = policy(agent)
  return projects.filter(project => access.projects === null || access.projects.includes(project.id))
}

export function taskProjects(agent: Agent, task: Pick<Task, 'projectId'>, projects: Project[]) {
  const allowed = allowedProjects(agent, projects)
  if (!task.projectId)
    return allowed
  const project = allowed.find(project => project.id === task.projectId)
  if (!project)
    throw new AppError(400, 'This project is unavailable to the selected agent.')
  return [project]
}

export function runProjects(run: Run) {
  return run.snapshot.projects ?? (run.snapshot.project ? [run.snapshot.project] : [])
}

export function validateAccess(agent: Agent) {
  const access = policy(agent)
  if (access.projects !== null && access.github)
    throw new AppError(400, 'Shared GitHub credentials require access to all projects. Disable the GitHub connection for an agent with selected projects.')
}
