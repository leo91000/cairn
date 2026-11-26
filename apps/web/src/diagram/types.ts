// Shared diagram AST types. Limits come from packages/contracts/diagram-spec.json.
export { DIAGRAM_LIMITS } from '../../../../packages/contracts/generated/diagram-spec'

export type Direction = 'TB' | 'BT' | 'LR' | 'RL'

export type NodeShape
  = | 'rect'
    | 'round'
    | 'stadium'
    | 'subroutine'
    | 'cylinder'
    | 'circle'
    | 'doublecircle'
    | 'diamond'
    | 'hexagon'
    | 'parallelogram'
    | 'parallelogram-alt'
    | 'trapezoid'
    | 'trapezoid-alt'
    | 'flag'
    | 'note'
    | 'start'
    | 'end'
    | 'fork'
    | 'choice'

/** Author colors are kept only when they are plain CSS colors. */
export interface DiagramStyle {
  fill?: string
  stroke?: string
  color?: string
  strokeWidth?: number
  dashed?: boolean
}

export interface GraphNode {
  id: string
  label: string
  shape: NodeShape
  parent?: string
  style?: DiagramStyle
  link?: string
  annotation?: { target: string, side: 'left' | 'right' }
}

export type EdgeLine = 'solid' | 'dotted' | 'thick' | 'invisible'
export type EdgeHead = 'none' | 'arrow' | 'circle' | 'cross'

export interface GraphEdge {
  from: string
  to: string
  label: string
  line: EdgeLine
  start: EdgeHead
  end: EdgeHead
  /** Extra dashes request a longer edge, as in Mermaid. */
  length: number
  style?: DiagramStyle
}

export interface GraphCluster {
  id: string
  label: string
  parent?: string
  style?: DiagramStyle
}

export interface GraphDiagram {
  kind: 'graph'
  title?: string
  direction: Direction
  nodes: GraphNode[]
  edges: GraphEdge[]
  clusters: GraphCluster[]
}

export interface Participant {
  id: string
  label: string
  actor: boolean
}

export type MessageHead = 'arrow' | 'open' | 'cross' | 'async' | 'none'

export type SequenceItem
  = | {
    type: 'message'
    from: string
    to: string
    label: string
    line: 'solid' | 'dotted'
    head: MessageHead
    both: boolean
    activate: boolean
    deactivate: boolean
    number?: number
  }
  | {
    type: 'note'
    placement: 'left' | 'right' | 'over'
    participants: string[]
    label: string
  }
  | { type: 'activate' | 'deactivate', participant: string }
  | {
    type: 'block'
    kind: BlockKind
    label: string
    color?: string
  }
  | { type: 'branch', kind: 'else' | 'and' | 'option', label: string }
  | { type: 'end' }

export type BlockKind = 'loop' | 'alt' | 'opt' | 'par' | 'critical' | 'break' | 'rect'

export interface SequenceDiagram {
  kind: 'sequence'
  title?: string
  participants: Participant[]
  items: SequenceItem[]
}

export interface PieDiagram {
  kind: 'pie'
  title?: string
  showData: boolean
  slices: { label: string, value: number }[]
}

export type Diagram = GraphDiagram | SequenceDiagram | PieDiagram

export class DiagramError extends Error {}
