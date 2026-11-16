import type { RuleNode } from '../../../packages/contracts/generated/typescript/RuleNode'

export const ruleGraphNodeWidth = 240
export const ruleGraphBranchGap = 64
export function ruleGraphWidth(nodes: RuleNode[]): number {
  return Math.max(ruleGraphNodeWidth, ...nodes.map(node => node.type === 'if'
    ? ruleGraphWidth(node.then) + ruleGraphBranchGap + ruleGraphWidth(node.otherwise)
    : node.type === 'forEach' ? ruleGraphWidth(node.body) + 96 : ruleGraphNodeWidth))
}

export function ruleGraphPorts(left: number, right: number) {
  const width = left + ruleGraphBranchGap + right
  return { left: left / 2 / width * 100, right: (left + ruleGraphBranchGap + right / 2) / width * 100 }
}
