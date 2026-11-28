import type {
  NodeBox,
  Paint,
  PathCommand,
  Scene,
  SceneItem,
  TextBlock,
  TextMeasure,
} from './scene'
import type { GraphDiagram, GraphEdge, GraphNode } from './types'
import {
  CLUSTER_FONT,
  CLUSTER_PAD,
  EDGE_SEP,
  family,
  fitScene,
  headInset,
  headPaths,
  LABEL_FONT,
  MARGIN,
  MAX_LAYOUT_CONSTRAINTS,
  MAX_LAYOUT_NODES,
  NODE_FONT,
  NODE_SEP,
  nodeBox,
  outlineExtents,
  RANK_GAP,
  roundedRect,
  shapePaths,
  textBlock,
  textOn,
  TITLE_FONT,
} from './scene'
import { DiagramError } from './types'

// ---------------------------------------------------------------------------
// Layered graph layout (flowcharts and state diagrams).

type LKind = 'node' | 'dummy' | 'label' | 'left' | 'right'

interface LNode {
  key: string
  kind: LKind
  w: number
  h: number
  /** Innermost containing cluster. */
  cluster?: string
  rank: number
  order: number
  x: number
  y: number
  /** Extra room on the right for self loops. */
  extra: number
  /** Extra rank-axis space for loop labels and horizontal cluster titles, without moving ports. */
  rankPadding?: number
  node?: GraphNode
  box?: NodeBox
}

interface LEdge {
  edge: GraphEdge
  index: number
  u: string
  v: string
  minlen: number
  reversed: boolean
  label?: TextBlock
  chain: LNode[]
}

interface Box {
  x: number
  y: number
  w: number
  h: number
}

export function layoutGraph(graph: GraphDiagram, measure: TextMeasure): Scene {
  if (!graph.nodes.length && !graph.clusters.length)
    throw new DiagramError('This diagram has nothing to draw.')
  const horizontal = graph.direction === 'LR' || graph.direction === 'RL'
  const annotations = new Map<string, GraphNode[]>()
  const annotationBoxes = new Map<string, ReturnType<typeof nodeBox>>()
  for (const note of graph.nodes) {
    if (!note.annotation)
      continue
    const target = graph.nodes.find(node => node.id === note.annotation!.target && !node.annotation)
    if (!target || graph.edges.some(edge => edge.from === target.id && edge.to === target.id))
      throw new DiagramError('Notes on composite states or states with self transitions are not supported.')
    const attached = annotations.get(target.id) ?? []
    attached.push(note)
    annotations.set(target.id, attached)
    annotationBoxes.set(note.id, nodeBox(note, horizontal, measure))
  }

  const clusters = new Map(graph.clusters.map(cluster => [cluster.id, cluster]))
  const chains = new Map<string, string[]>()
  const chainOf = (id: string | undefined) => {
    if (!id)
      return []
    let chain = chains.get(id)
    if (!chain) {
      chain = []
      for (let current: string | undefined = id; current && clusters.has(current); current = clusters.get(current)!.parent)
        chain.unshift(current)
      chains.set(id, chain)
    }

    return chain
  }

  // Layout nodes, in layout space: x across ranks, y along them.
  const nodes = new Map<string, LNode>()
  const add = (node: LNode) => {
    if (!nodes.has(node.key) && nodes.size >= MAX_LAYOUT_NODES)
      throw new DiagramError('This diagram is too large to draw.')
    nodes.set(node.key, node)
    return node
  }

  for (const node of graph.nodes) {
    if (node.annotation)
      continue
    const box = nodeBox(node, horizontal, measure)
    const notes = annotations.get(node.id) ?? []
    const sideHeight = (side: 'left' | 'right') => notes.filter(note => note.annotation!.side === side)
      .reduce((height, note) => height + annotationBoxes.get(note.id)!.h + 12, -12)
    const noteWidth = Math.max(0, ...notes.map(note => annotationBoxes.get(note.id)!.w + 28))
    const reservedWidth = box.w + noteWidth * 2
    const reservedHeight = Math.max(box.h, sideHeight('left'), sideHeight('right'))
    add({
      key: node.id,
      kind: 'node',
      w: horizontal ? reservedHeight : reservedWidth,
      h: horizontal ? reservedWidth : reservedHeight,
      cluster: node.parent,
      rank: 0,
      order: 0,
      x: 0,
      y: 0,
      extra: 0,
      node,
      box,
    })
  }

  const titles = new Map<string, TextBlock>()
  for (const cluster of graph.clusters)
    titles.set(cluster.id, textBlock(cluster.label, CLUSTER_FONT, 600, 240, measure))

  // An empty cluster still draws its box around a placeholder.
  function append<K, V>(map: Map<K, V[]>, key: K, value: V) {
    const values = map.get(key)
    if (values)
      values.push(value)
    else
      map.set(key, [value])
  }

  const members = new Map<string, string[]>()
  for (const node of graph.nodes) {
    if (node.annotation)
      continue
    for (const id of chainOf(node.parent))
      append(members, id, node.id)
  }

  for (const cluster of graph.clusters) {
    if (members.has(cluster.id))
      continue
    const key = `\u0000empty:${cluster.id}`
    const title = titles.get(cluster.id)!
    add({
      key,
      kind: 'dummy',
      w: horizontal ? 24 : Math.max(title.width, 48),
      h: 24,
      cluster: cluster.id,
      rank: 0,
      order: 0,
      x: 0,
      y: 0,
      extra: 0,
    })
    for (const id of chainOf(cluster.id))
      append(members, id, key)
  }

  // Rank constraints. A link to or from a subgraph constrains every member, so the whole
  // subgraph sits before or after the other end, as its box will be drawn.
  interface Constraint { u: string, v: string, minlen: number }

  const constraints: Constraint[] = []
  const ends = (id: string) => clusters.has(id) ? members.get(id)! : [id]
  const links: {
    edge: GraphEdge
    index: number
    label?: TextBlock
    back: boolean
  }[] = []
  const loops: {
    edge: GraphEdge
    index: number
    node: LNode
    label?: TextBlock
    reach: number
  }[] = []
  const nextLoopReach = new Map<string, number>()
  graph.edges.forEach((edge, index) => {
    const label = edge.label ? textBlock(edge.label, LABEL_FONT, 500, 160, measure) : undefined
    if (edge.from === edge.to) {
      const node = nodes.get(edge.from)
      if (node) {
        const reach = nextLoopReach.get(node.key) ?? 24 + Math.min(node.h, 40) / 2
        const labelExtent = label ? horizontal ? label.height : label.width : 0
        const rankExtent = label ? horizontal ? label.width : label.height : 0
        nextLoopReach.set(node.key, reach + Math.max(32, (labelExtent + 16) / 0.75))
        node.extra = Math.max(node.extra, reach + 12, reach * 0.75 + labelExtent + 18)
        node.rankPadding = Math.max(node.rankPadding ?? 0, (rankExtent - node.h) / 2 + 6)
        loops.push({
          edge,
          index,
          node,
          label,
          reach,
        })
      }

      return
    }

    links.push({
      edge,
      index,
      label,
      back: false,
    })
  })

  // Break cycles twice: first between links as written, with subgraphs as single ends, so a
  // link back into a subgraph is the one reversed; then between the members it expands to.
  const linkState = new Map<string, 1 | 2>()
  const linksFrom = new Map<string, typeof links>()
  for (const link of links)
    append(linksFrom, link.edge.from, link)
  for (const start of [...graph.nodes.map(node => node.id), ...clusters.keys()]) {
    if (linkState.has(start))
      continue
    const stack: [string, number][] = [[start, 0]]
    linkState.set(start, 1)
    while (stack.length) {
      const top = stack.at(-1)!
      const list = linksFrom.get(top[0]) ?? []
      if (top[1] >= list.length) {
        linkState.set(top[0], 2)
        stack.pop()
        continue
      }

      const link = list[top[1]++]!
      const seen = linkState.get(link.edge.to)
      if (seen === 1) {
        link.back = true
      }
      else if (!seen) {
        linkState.set(link.edge.to, 1)
        stack.push([link.edge.to, 0])
      }
    }
  }

  for (const link of links) {
    const [from, to] = link.back ? [link.edge.to, link.edge.from] : [link.edge.from, link.edge.to]
    for (const u of ends(from)) {
      for (const v of ends(to)) {
        if (u !== v) {
          if (constraints.length >= MAX_LAYOUT_CONSTRAINTS)
            throw new DiagramError('This diagram has too many expanded links to draw.')
          constraints.push({ u, v, minlen: link.edge.length * 2 })
        }
      }
    }
  }

  const outgoing = new Map<string, Constraint[]>()
  for (const constraint of constraints)
    append(outgoing, constraint.u, constraint)
  const state = new Map<string, 1 | 2>()
  const backward = new Set<Constraint>()
  for (const start of nodes.keys()) {
    if (state.has(start))
      continue
    const stack: [string, number][] = [[start, 0]]
    state.set(start, 1)
    while (stack.length) {
      const top = stack.at(-1)!
      const list = outgoing.get(top[0]) ?? []
      if (top[1] >= list.length) {
        state.set(top[0], 2)
        stack.pop()
        continue
      }

      const constraint = list[top[1]++]!
      const seen = state.get(constraint.v)
      if (seen === 1) {
        backward.add(constraint)
      }
      else if (!seen) {
        state.set(constraint.v, 1)
        stack.push([constraint.v, 0])
      }
    }
  }

  for (const constraint of backward)
    [constraint.u, constraint.v] = [constraint.v, constraint.u]

  // Rank by longest path, then pull nodes with more outgoing than incoming constraints down.
  const ins = new Map<string, Constraint[]>()
  const outs = new Map<string, Constraint[]>()
  for (const constraint of constraints) {
    append(ins, constraint.v, constraint)
    append(outs, constraint.u, constraint)
  }

  const indegree = new Map([...nodes.keys()].map(key => [key, ins.get(key)?.length ?? 0]))
  const queue = [...nodes.keys()].filter(key => !indegree.get(key))
  const topo: string[] = []
  while (queue.length) {
    const key = queue.shift()!
    topo.push(key)
    for (const constraint of outs.get(key) ?? []) {
      const next = nodes.get(constraint.v)!
      next.rank = Math.max(next.rank, nodes.get(key)!.rank + constraint.minlen)
      indegree.set(constraint.v, indegree.get(constraint.v)! - 1)
      if (!indegree.get(constraint.v))
        queue.push(constraint.v)
    }
  }

  for (const key of topo.toReversed()) {
    const out = outs.get(key) ?? []
    if (!out.length || out.length <= (ins.get(key)?.length ?? 0))
      continue
    const node = nodes.get(key)!
    node.rank = Math.min(...out.map(constraint => nodes.get(constraint.v)!.rank - constraint.minlen))
  }

  const lowest = Math.min(...[...nodes.values()].map(node => node.rank))
  for (const node of nodes.values())
    node.rank -= lowest

  // With ranks known, a subgraph's link attaches to its member nearest the other end.
  const nearest = (id: string, rank: number) => {
    if (!clusters.has(id))
      return id
    let best = members.get(id)![0]!
    for (const key of members.get(id)!) {
      if (Math.abs(nodes.get(key)!.rank - rank) < Math.abs(nodes.get(best)!.rank - rank))
        best = key
    }

    return best
  }

  const edges: LEdge[] = []
  const flats: LEdge[] = []
  for (const { edge, index, label } of links) {
    const middle = (id: string) => {
      const ranks = ends(id).map(key => nodes.get(key)!.rank)
      return ranks.reduce((sum, rank) => sum + rank, 0) / ranks.length
    }

    const u = nearest(edge.from, middle(edge.to))
    const v = nearest(edge.to, nodes.get(u)!.rank)
    if (u === v)
      continue
    const reversed = nodes.get(u)!.rank > nodes.get(v)!.rank
    const link: LEdge = {
      edge,
      index,
      u: reversed ? v : u,
      v: reversed ? u : v,
      minlen: edge.length * 2,
      reversed,
      label,
      chain: [],
    }
    if (nodes.get(u)!.rank === nodes.get(v)!.rank)
      flats.push(link)
    else
      edges.push(link)
  }

  // Each edge becomes a chain of dummies, one per intermediate rank; labels take the middle one.
  const lca = (a: string | undefined, b: string | undefined) => {
    const left = chainOf(a)
    const right = chainOf(b)
    let common: string | undefined
    for (let index = 0; index < Math.min(left.length, right.length) && left[index] === right[index]; index++)
      common = left[index]
    return common
  }

  for (const edge of edges) {
    const u = nodes.get(edge.u)!
    const v = nodes.get(edge.v)!
    const cluster = lca(u.cluster, v.cluster)
    const middle = u.rank + Math.floor((v.rank - u.rank) / 2)
    edge.chain = [u]
    for (let rank = u.rank + 1; rank < v.rank; rank++) {
      const labelled = edge.label && rank === middle
      const label = edge.label!
      edge.chain.push(add({
        key: `\u0000edge:${edge.index}:${rank}`,
        kind: labelled ? 'label' : 'dummy',
        w: labelled ? (horizontal ? label.height + 8 : label.width + 14) : 0,
        h: labelled ? (horizontal ? label.width + 14 : label.height + 8) : 0,
        cluster,
        rank,
        order: 0,
        x: 0,
        y: 0,
        extra: 0,
      }))
    }

    edge.chain.push(v)
  }

  // Clusters keep a left and right border node on every rank they span.
  const spans = new Map<string, [number, number]>()
  for (const node of nodes.values()) {
    for (const id of chainOf(node.cluster)) {
      const span = spans.get(id)
      spans.set(id, span ? [Math.min(span[0], node.rank), Math.max(span[1], node.rank)] : [node.rank, node.rank])
    }
  }

  for (const [id, [first, last]] of spans) {
    for (let rank = first; rank <= last; rank++) {
      for (const kind of ['left', 'right'] as const) {
        add({
          key: `\u0000${kind}:${id}:${rank}`,
          kind,
          w: 0,
          h: 0,
          cluster: id,
          rank,
          order: 0,
          x: 0,
          y: 0,
          extra: 0,
        })
      }
    }
  }

  const rankCount = Math.max(0, ...[...nodes.values()].map(node => node.rank)) + 1
  const layers: LNode[][] = Array.from({ length: rankCount }, () => [])
  const neighbors = new Map<string, { up: LNode[], down: LNode[] }>()
  for (const node of nodes.values())
    neighbors.set(node.key, { up: [], down: [] })
  for (const edge of edges) {
    for (let index = 1; index < edge.chain.length; index++) {
      neighbors.get(edge.chain[index - 1]!.key)!.down.push(edge.chain[index]!)
      neighbors.get(edge.chain[index]!.key)!.up.push(edge.chain[index - 1]!)
    }
  }

  // Initial order: depth-first from the declared nodes, so authored order mostly survives.
  const visited = new Set<string>()
  const visit = (start: LNode) => {
    const stack = [start]
    while (stack.length) {
      const node = stack.pop()!
      if (visited.has(node.key))
        continue
      visited.add(node.key)
      layers[node.rank]!.push(node)
      stack.push(...neighbors.get(node.key)!.down.toReversed())
    }
  }

  for (const node of nodes.values()) {
    if (node.kind === 'node' || node.kind === 'dummy')
      visit(node)
  }

  for (const node of nodes.values()) {
    if (!visited.has(node.key))
      visit(node)
  }

  const clusterDepth = (id: string | undefined) => chainOf(id).length
  const siblingOrder = new Map<string, number>()

  /** Sort one layer by key, keeping each cluster contiguous and bordered. */
  const arrange = (layer: LNode[], key: (node: LNode) => number) => {
    const place = (items: LNode[], depth: number, inside: string | undefined): LNode[] => {
      interface Entry { key: number, nodes: LNode[], cluster?: string }

      const entries: Entry[] = []
      const groups = new Map<string, LNode[]>()
      let left: LNode | undefined
      let right: LNode | undefined
      for (const node of items) {
        const chain = chainOf(node.cluster)
        if (chain.length === depth) {
          if (node.kind === 'left' && node.cluster === inside)
            left = node
          else if (node.kind === 'right' && node.cluster === inside)
            right = node
          else
            entries.push({ key: key(node), nodes: [node] })
        }
        else {
          const child = chain[depth]!
          groups.set(child, [...(groups.get(child) ?? []), node])
        }
      }

      for (const [cluster, group] of groups) {
        const keyed = group.filter(node => node.kind !== 'left' && node.kind !== 'right')
        const value = keyed.length ? keyed.reduce((sum, node) => sum + key(node), 0) / keyed.length : key(group[0]!)
        entries.push({ key: value, nodes: place(group, depth + 1, cluster), cluster })
      }

      entries.sort((a, b) => a.key - b.key)
      // Sibling clusters keep one relative order on every rank, or their boxes would overlap.
      const slots = entries.map((entry, index) => entry.cluster ? index : -1).filter(index => index >= 0)
      const ordered = slots.map(index => entries[index]!).sort((a, b) => (siblingOrder.get(a.cluster!) ?? 0) - (siblingOrder.get(b.cluster!) ?? 0))
      slots.forEach((slot, index) => {
        entries[slot] = ordered[index]!
      })
      return [...(left ? [left] : []), ...entries.flatMap(entry => entry.nodes), ...(right ? [right] : [])]
    }

    const sorted = place(layer, 0, undefined)
    sorted.forEach((node, index) => {
      node.order = index
    })
    return sorted
  }

  const updateSiblingOrder = () => {
    const totals = new Map<string, [number, number]>()
    for (const layer of layers) {
      for (const node of layer) {
        if (!node.cluster)
          continue
        for (const id of chainOf(node.cluster)) {
          const total = totals.get(id) ?? [0, 0]
          totals.set(id, [total[0] + node.order / Math.max(1, layer.length - 1), total[1] + 1])
        }
      }
    }

    for (const [id, [sum, count]] of totals)
      siblingOrder.set(id, sum / count)
  }

  // Crossings between two layers are inversions of the lower ends, counted with a Fenwick tree.
  const crossings = () => {
    let count = 0
    for (let rank = 0; rank + 1 < layers.length; rank++) {
      const ends: number[] = []
      for (const node of layers[rank]!) {
        const lower = neighbors.get(node.key)!.down.map(next => next.order).sort((a, b) => a - b)
        ends.push(...lower)
      }

      const size = layers[rank + 1]!.length + 1
      const tree = Array.from<number>({ length: size + 1 }).fill(0)
      for (const [index, end] of ends.entries()) {
        let below = 0
        for (let at = end + 1; at > 0; at -= at & -at)
          below += tree[at]!
        count += index - below
        for (let at = end + 1; at <= size; at += at & -at)
          tree[at]!++
      }
    }

    return count
  }

  for (const layer of layers) {
    layer.forEach((node, index) => {
      node.order = index
    })
  }

  updateSiblingOrder()
  for (const [rank, layer] of layers.entries())
    layers[rank] = arrange(layer, node => node.order)

  let best = layers.map(layer => [...layer])
  let bestCrossings = crossings()
  const barycenter = (node: LNode, side: 'up' | 'down') => {
    const list = neighbors.get(node.key)![side]
    if (!list.length)
      return node.order
    return list.reduce((sum, next) => sum + next.order, 0) / list.length
  }

  for (let iteration = 0; iteration < 16 && bestCrossings > 0; iteration++) {
    const down = iteration % 2 === 0
    updateSiblingOrder()
    const ranks = [...layers.keys()]
    for (const rank of down ? ranks.slice(1) : ranks.toReversed().slice(1))
      layers[rank] = arrange(layers[rank]!, node => barycenter(node, down ? 'up' : 'down'))
    const count = crossings()
    if (count < bestCrossings) {
      bestCrossings = count
      best = layers.map(layer => [...layer])
    }
  }

  for (const [rank, layer] of best.entries()) {
    layers[rank] = layer
    layer.forEach((node, index) => {
      node.order = index
    })
  }

  // Horizontal positions. Separation depends on what sits side by side.
  const separation = (a: LNode, b: LNode) => {
    const half = (a.w + b.w) / 2 + a.extra
    if (a.kind === 'left' && b.cluster === a.cluster && b.kind !== 'right')
      return half + CLUSTER_PAD + (horizontal ? titles.get(a.cluster!)!.height + 6 : 0)
    if (a.kind === 'left' || b.kind === 'right')
      return half + CLUSTER_PAD
    if (a.kind === 'right' || b.kind === 'left')
      return half + (a.kind === 'right' && b.kind === 'left' ? 14 : 20)
    if (a.kind === 'dummy' || b.kind === 'dummy')
      return half + EDGE_SEP
    return half + NODE_SEP
  }

  const weight = (a: LNode, b: LNode) => {
    const dummyA = a.kind === 'dummy' || a.kind === 'label'
    const dummyB = b.kind === 'dummy' || b.kind === 'label'
    return dummyA && dummyB ? 8 : dummyA || dummyB ? 2 : 1
  }

  const pushRight = () => {
    for (const layer of layers) {
      for (let index = 1; index < layer.length; index++) {
        const minimum = layer[index - 1]!.x + separation(layer[index - 1]!, layer[index]!)
        if (layer[index]!.x < minimum)
          layer[index]!.x = minimum
      }
    }
  }

  pushRight()

  /** Pool-adjacent-violators: closest positions to the targets that keep the separations. */
  const solve = (layer: LNode[], targets: number[], weights: number[]) => {
    const offsets = [0]
    for (let index = 1; index < layer.length; index++)
      offsets.push(offsets[index - 1]! + separation(layer[index - 1]!, layer[index]!))
    const blocks: { weight: number, total: number, count: number }[] = []
    for (let index = 0; index < layer.length; index++) {
      blocks.push({ weight: weights[index]!, total: weights[index]! * (targets[index]! - offsets[index]!), count: 1 })
      while (blocks.length > 1) {
        const last = blocks.at(-1)!
        const previous = blocks.at(-2)!
        if (previous.total / previous.weight <= last.total / last.weight)
          break
        blocks.pop()
        previous.weight += last.weight
        previous.total += last.total
        previous.count += last.count
      }
    }

    let index = 0
    for (const block of blocks) {
      const value = block.total / block.weight
      for (let item = 0; item < block.count; item++, index++)
        layer[index]!.x = value + offsets[index]!
    }
  }

  for (let iteration = 0; iteration < 10; iteration++) {
    const down = iteration % 2 === 0
    const ranks = [...layers.keys()]
    for (const rank of down ? ranks : ranks.toReversed()) {
      const layer = layers[rank]!
      const targets: number[] = []
      const weights: number[] = []
      for (const node of layer) {
        const links = neighbors.get(node.key)!
        const primary = down ? links.up : links.down
        const list = primary.length ? primary : (down ? links.down : links.up)
        if (!list.length) {
          targets.push(node.x)
          weights.push(node.kind === 'left' || node.kind === 'right' ? 0.25 : 0.5)
          continue
        }

        let total = 0
        let sum = 0
        for (const next of list) {
          const w = weight(node, next)
          total += w
          sum += w * next.x
        }

        targets.push(sum / total)
        weights.push(total)
      }

      solve(layer, targets, weights)
    }
  }

  // A border has one position across every rank. Solve their shared separation constraints
  // together so aligning one group cannot undo another rank's clearance.
  const borders = new Map<string, { left: LNode[], right: LNode[] }>()
  for (const node of nodes.values()) {
    if (node.kind !== 'left' && node.kind !== 'right')
      continue
    const entry = borders.get(node.cluster!) ?? { left: [], right: [] }
    entry[node.kind].push(node)
    borders.set(node.cluster!, entry)
  }

  const minimumWidth = (id: string) => (horizontal ? 0 : titles.get(id)!.width + CLUSTER_PAD * 2)
  const representative = (node: LNode) => {
    if (node.kind === 'left' || node.kind === 'right')
      return borders.get(node.cluster!)![node.kind][0]!
    return node
  }

  const borderPositions = new Map<LNode, number>()
  const borderIncoming = new Map<LNode, number>()
  const borderOutgoing = new Map<LNode, { node: LNode, distance: number }[]>()
  for (const node of nodes.values()) {
    const key = representative(node)
    const existing = borderPositions.get(key)
    borderPositions.set(key, existing === undefined ? node.x : node.kind === 'left' ? Math.min(existing, node.x) : Math.max(existing, node.x))
    borderIncoming.set(key, 0)
    borderOutgoing.set(key, [])
  }

  const constrain = (left: LNode, right: LNode, distance: number) => {
    const from = representative(left)
    const to = representative(right)
    borderOutgoing.get(from)!.push({ node: to, distance })
    borderIncoming.set(to, borderIncoming.get(to)! + 1)
  }

  for (const layer of layers) {
    for (let index = 1; index < layer.length; index++)
      constrain(layer[index - 1]!, layer[index]!, separation(layer[index - 1]!, layer[index]!))
  }

  for (const [id, { left, right }] of borders)
    constrain(left[0]!, right[0]!, minimumWidth(id))

  const borderReady = [...borderPositions.keys()].filter(node => borderIncoming.get(node) === 0)
  for (let index = 0; index < borderReady.length; index++) {
    const from = borderReady[index]!
    for (const { node, distance } of borderOutgoing.get(from)!) {
      borderPositions.set(node, Math.max(borderPositions.get(node)!, borderPositions.get(from)! + distance))
      const remaining = borderIncoming.get(node)! - 1
      borderIncoming.set(node, remaining)
      if (!remaining)
        borderReady.push(node)
    }
  }

  if (borderReady.length !== borderPositions.size)
    throw new DiagramError('These groups cannot be separated without overlap.')
  for (const node of nodes.values())
    node.x = borderPositions.get(representative(node))!

  // Straighten long edges where their layers leave room for a direct line.
  const fits = (node: LNode, x: number) => {
    const layer = layers[node.rank]!
    const left = layer[node.order - 1]
    const right = layer[node.order + 1]
    return (!left || left.x + separation(left, node) <= x + 0.01) && (!right || x + separation(node, right) <= right.x + 0.01)
  }

  for (let pass = 0; pass < 2; pass++) {
    for (const edge of edges) {
      const inner = edge.chain.slice(1, -1)
      if (!inner.length)
        continue
      const a = edge.chain[0]!
      const b = edge.chain.at(-1)!
      const target = (node: LNode) => a.x + (b.x - a.x) * (node.rank - a.rank) / (b.rank - a.rank)
      if (inner.every(node => fits(node, target(node)))) {
        for (const node of inner)
          node.x = target(node)
      }
    }
  }

  // Center each subgraph's content when its title made the box wider.
  for (const cluster of [...graph.clusters].sort((a, b) => clusterDepth(b.id) - clusterDepth(a.id))) {
    const entry = borders.get(cluster.id)
    if (!entry)
      continue
    const inside = [...nodes.values()].filter(node => node.cluster && chainOf(node.cluster).includes(cluster.id) && !((node.kind === 'left' || node.kind === 'right') && node.cluster === cluster.id))
    if (!inside.length)
      continue
    const contentLeft = Math.min(...inside.map(node => node.x - node.w / 2))
    const contentRight = Math.max(...inside.map(node => node.x + node.w / 2 + node.extra))
    const leftRoom = contentLeft - entry.left[0]!.x - CLUSTER_PAD - (horizontal ? titles.get(cluster.id)!.height + 6 : 0)
    const rightRoom = entry.right[0]!.x - contentRight - CLUSTER_PAD
    let shift = (rightRoom - leftRoom) / 2
    const moving = new Set(inside)
    // Every rank keeps its separation constraints when the whole cluster moves together.
    for (const node of inside) {
      const right = layers[node.rank]![node.order + 1]
      if (right && !moving.has(right))
        shift = Math.min(shift, right.x - node.x - separation(node, right))
    }

    if (shift > 0.5) {
      for (const node of inside)
        node.x += shift
    }
  }

  // Horizontal titles need space along the rank axis, including in empty/nested clusters.
  if (horizontal) {
    for (const node of nodes.values()) {
      if (node.kind === 'left' || node.kind === 'right')
        continue
      for (const id of chainOf(node.cluster)) {
        if (spans.get(id)![0] === node.rank)
          node.rankPadding = Math.max(node.rankPadding ?? 0, (titles.get(id)!.width - node.h) / 2)
      }
    }
  }

  // Vertical positions: rank gaps grow where clusters open or close.
  const rankHeight = layers.map(layer => Math.max(0, ...layer.map(node => node.h + (node.rankPadding ?? 0) * 2)))
  const opening = Array.from<number>({ length: rankCount }).fill(0)
  const closing = Array.from<number>({ length: rankCount }).fill(0)
  for (const layer of layers) {
    for (const node of layer) {
      let opens = 0
      let closes = 0
      for (const id of chainOf(node.cluster)) {
        const [first, last] = spans.get(id)!
        if (first === node.rank)
          opens += CLUSTER_PAD + (graph.direction === 'TB' ? titles.get(id)!.height + 4 : 0)
        if (last === node.rank)
          closes += CLUSTER_PAD + (graph.direction === 'BT' ? titles.get(id)!.height + 4 : 0)
      }

      opening[node.rank] = Math.max(opening[node.rank]!, opens)
      closing[node.rank] = Math.max(closing[node.rank]!, closes)
    }
  }

  const rankY: number[] = []
  let cursor = 0
  for (let rank = 0; rank < rankCount; rank++) {
    cursor += (rank ? RANK_GAP : 0) + opening[rank]!
    rankY.push(cursor + rankHeight[rank]! / 2)
    cursor += rankHeight[rank]! + closing[rank]!
  }

  for (const node of nodes.values())
    node.y = rankY[node.rank]!

  // Notes reserve space with their target, then sit on the requested physical side in every direction.
  for (const [target, notes] of annotations) {
    const node = nodes.get(target)!
    for (const side of ['left', 'right'] as const) {
      const attached = notes.filter(note => note.annotation!.side === side)
      const total = attached.reduce((height, note) => height + annotationBoxes.get(note.id)!.h + 12, -12)
      let y = -total / 2
      for (const note of attached) {
        const box = annotationBoxes.get(note.id)!
        const dx = (side === 'left' ? -1 : 1) * (node.box!.w / 2 + 28 + box.w / 2)
        const dy = y + box.h / 2
        const offset: [number, number] = graph.direction === 'BT'
          ? [dx, -dy]
          : graph.direction === 'LR'
            ? [dy, dx]
            : graph.direction === 'RL' ? [dy, -dx] : [dx, dy]
        add({
          key: note.id,
          kind: 'node',
          w: horizontal ? box.h : box.w,
          h: horizontal ? box.w : box.h,
          cluster: node.cluster,
          rank: node.rank,
          order: 0,
          x: node.x + offset[0],
          y: node.y + offset[1],
          extra: 0,
          node: note,
          box,
        })
        y += box.h + 12
      }
    }
  }

  // Cluster boxes, innermost first so parents can include their children.
  const boxes = new Map<string, Box>()
  const ordered = [...graph.clusters].sort((a, b) => clusterDepth(b.id) - clusterDepth(a.id))
  for (const cluster of ordered) {
    if (!spans.has(cluster.id))
      continue
    const { left, right } = borders.get(cluster.id)!
    let top = Infinity
    let bottom = -Infinity
    for (const node of nodes.values()) {
      if (node.kind === 'left' || node.kind === 'right' || !chainOf(node.cluster).includes(cluster.id))
        continue
      top = Math.min(top, node.y - node.h / 2 - (node.rankPadding ?? 0))
      bottom = Math.max(bottom, node.y + node.h / 2 + (node.rankPadding ?? 0))
    }

    for (const child of graph.clusters) {
      const box = child.parent === cluster.id ? boxes.get(child.id) : undefined
      if (box) {
        top = Math.min(top, box.y)
        bottom = Math.max(bottom, box.y + box.h)
      }
    }

    const title = titles.get(cluster.id)!.height + 4
    top -= CLUSTER_PAD + (graph.direction === 'TB' ? title : 0)
    bottom += CLUSTER_PAD + (graph.direction === 'BT' ? title : 0)
    boxes.set(cluster.id, {
      x: left[0]!.x,
      y: top,
      w: right[0]!.x - left[0]!.x,
      h: bottom - top,
    })
  }

  // Layout extents and the transform into the drawing's own direction.
  let minX = Infinity
  let maxX = -Infinity
  let maxY = 0
  for (const node of nodes.values()) {
    minX = Math.min(minX, node.x - node.w / 2)
    maxX = Math.max(maxX, node.x + node.w / 2 + node.extra)
    maxY = Math.max(maxY, node.y + node.h / 2 + (node.rankPadding ?? 0))
  }

  for (const box of boxes.values()) {
    minX = Math.min(minX, box.x)
    maxX = Math.max(maxX, box.x + box.w)
    maxY = Math.max(maxY, box.y + box.h)
  }

  const titleBlock = graph.title ? textBlock(graph.title, TITLE_FONT, 600, 480, measure) : undefined
  const titleSpace = titleBlock ? titleBlock.height + 14 : 0
  const extentX = maxX - minX
  const extentY = maxY
  const graphWidth = (horizontal ? extentY : extentX) + MARGIN * 2
  const titleWidth = titleBlock ? titleBlock.width + MARGIN * 2 : 0
  const horizontalMargin = MARGIN + Math.max(0, (titleWidth - graphWidth) / 2)
  const transform = ([x, y]: [number, number]): [number, number] => {
    const lx = x - minX
    switch (graph.direction) {
      case 'BT':
        return [lx + horizontalMargin, extentY - y + MARGIN + titleSpace]
      case 'LR':
        return [y + horizontalMargin, lx + MARGIN + titleSpace]
      case 'RL':
        return [extentY - y + horizontalMargin, lx + MARGIN + titleSpace]
      default:
        return [lx + horizontalMargin, y + MARGIN + titleSpace]
    }
  }

  const width = Math.ceil(Math.max(graphWidth, titleWidth))
  const height = Math.ceil((horizontal ? extentX : extentY) + MARGIN * 2 + titleSpace)
  const finalBox = (box: Box): Box => {
    const [x0, y0] = transform([box.x, box.y])
    const [x1, y1] = transform([box.x + box.w, box.y + box.h])
    return {
      x: Math.min(x0, x1),
      y: Math.min(y0, y1),
      w: Math.abs(x1 - x0),
      h: Math.abs(y1 - y0),
    }
  }

  const items: SceneItem[] = []
  const backgrounds: SceneItem[] = []
  const edgeItems: SceneItem[] = []
  const labelItems: SceneItem[] = []
  const titleItems: SceneItem[] = []
  const nodeItems: SceneItem[] = []

  for (const [target, notes] of annotations) {
    const node = nodes.get(target)!
    const [cx, cy] = transform([node.x, node.y])
    for (const note of notes) {
      const at = nodes.get(note.id)!
      const [nx, ny] = transform([at.x, at.y])
      const sign = note.annotation!.side === 'left' ? -1 : 1
      edgeItems.push({
        kind: 'path',
        d: [['M', cx + sign * node.box!.w / 2, cy], ['L', nx - sign * at.box!.w / 2, ny]],
        stroke: { tone: 'edge' },
        width: 1,
        dash: [4, 4],
      })
    }
  }

  if (titleBlock) {
    items.push({
      kind: 'text',
      x: width / 2,
      y: MARGIN + titleBlock.height / 2,
      lines: titleBlock.lines,
      size: TITLE_FONT,
      weight: 600,
      anchor: 'middle',
      fill: { tone: 'ink' },
    })
  }

  for (const cluster of [...graph.clusters].sort((a, b) => clusterDepth(a.id) - clusterDepth(b.id))) {
    const box = boxes.get(cluster.id)
    if (!box)
      continue
    const area = finalBox(box)
    backgrounds.push({
      kind: 'path',
      d: roundedRect(area.x, area.y, area.w, area.h, 14),
      fill: cluster.style?.fill ? { color: cluster.style.fill } : { tone: 'cluster' },
      stroke: cluster.style?.stroke ? { color: cluster.style.stroke } : { tone: 'cluster-line' },
      width: cluster.style?.strokeWidth ?? 1,
      ...(cluster.style?.dashed ? { dash: [5, 4] } : {}),
    })
    // Titles sit above edges, on a patch of the box's own fill, so lines never cross them.
    const title = titles.get(cluster.id)!
    if (title.lines.length) {
      const x = area.x + CLUSTER_PAD - 2
      const y = area.y + 10 + title.height / 2
      titleItems.push(
        { kind: 'path', d: roundedRect(x - 4, y - title.height / 2 - 1, title.width + 8, title.height + 2, 5), fill: cluster.style?.fill ? { color: cluster.style.fill } : { tone: 'cluster' } },
        {
          kind: 'text',
          x,
          y,
          lines: title.lines,
          size: CLUSTER_FONT,
          weight: 600,
          anchor: 'start',
          fill: textOn(cluster.style, 'cluster-ink'),
        },
      )
    }
  }

  // Edges are cubic segments with tangents along the rank axis, joined at chain points.
  const axis = (sign: number): [number, number] => {
    switch (graph.direction) {
      case 'BT':
        return [0, -sign]
      case 'LR':
        return [sign, 0]
      case 'RL':
        return [-sign, 0]
      default:
        return [0, sign]
    }
  }

  const edgePaint = (edge: GraphEdge): Paint => edge.style?.stroke ? { color: edge.style.stroke } : { tone: 'edge' }
  const edgeWidth = (edge: GraphEdge) => edge.style?.strokeWidth ?? (edge.line === 'thick' ? 3 : 1.5)

  const nodeOutlines = new Map<string, PathCommand[][]>()
  const outlineFor = (node: LNode) => {
    let paths = nodeOutlines.get(node.key)
    if (!paths) {
      paths = shapePaths(node.node!.shape, 0, 0, node.box!.w, node.box!.h).outlines
      nodeOutlines.set(node.key, paths)
    }

    return paths
  }

  for (const edge of edges) {
    const { edge: source } = edge
    if (source.line === 'invisible')
      continue
    const chain = edge.chain
    const first = chain[0]!
    const last = chain.at(-1)!
    const port = (node: LNode, toward: number, side: 1 | -1): [number, number] => {
      if (!node.node)
        return [node.x, node.y]
      const shape = node.node.shape
      const w = node.box ? horizontal ? node.box.h : node.box.w : node.w
      const round = shape === 'circle' || shape === 'doublecircle' || shape === 'diamond' || shape === 'choice' || shape === 'start' || shape === 'end'
      const reach = Math.max(0, round ? w * 0.2 : w / 2 - 12)
      const dx = Math.max(-reach, Math.min(reach, toward - node.x))
      const depth = outlineExtents(outlineFor(node), graph.direction, 'y', side, [dx])[0]!
      return [node.x + dx, node.y + side * depth]
    }

    let points: [number, number][] = chain.slice(1, -1).map(node => [node.x, node.y])
    const startPoint = port(first, (chain[1] ?? last).x, 1)
    const endPoint = port(last, (chain.at(-2) ?? first).x, -1)
    points = [startPoint, ...points, endPoint]

    // Links to a subgraph stop at its border.
    const fromCluster = edge.reversed ? source.to : source.from
    const toCluster = edge.reversed ? source.from : source.to
    if (clusters.has(toCluster) && boxes.has(toCluster)) {
      const box = boxes.get(toCluster)!
      const outside = points.filter(([, y]) => y < box.y)
      const previous = outside.at(-1) ?? points[0]!
      points = [...outside, [Math.max(box.x + 12, Math.min(box.x + box.w - 12, previous[0])), box.y]]
    }

    if (clusters.has(fromCluster) && boxes.has(fromCluster)) {
      const box = boxes.get(fromCluster)!
      const outside = points.filter(([, y]) => y > box.y + box.h)
      const next = outside[0] ?? points.at(-1)!
      points = [[Math.max(box.x + 12, Math.min(box.x + box.w - 12, next[0])), box.y + box.h], ...outside]
    }

    if (points.length < 2)
      continue
    const startHead = edge.reversed ? source.end : source.start
    const endHead = edge.reversed ? source.start : source.end
    // Pull the curve back so heads sit at the outline instead of under the stroke.
    const startInset = headInset(startHead)
    const endInset = headInset(endHead)
    const tipStart = points[0]!
    const tipEnd = points.at(-1)!
    points[0] = [tipStart[0], tipStart[1] + startInset]
    points[points.length - 1] = [tipEnd[0], tipEnd[1] - endInset]

    const d = smoothPath(points, transform)
    const paint = edgePaint(source)
    const width = edgeWidth(source)
    const dashed = source.line === 'dotted' || source.style?.dashed
    edgeItems.push({
      kind: 'path',
      d,
      stroke: paint,
      width,
      ...(dashed ? { dash: [4, 4] } : {}),
    })
    edgeItems.push(...headPaths(endHead, transform(tipEnd), axis(1), paint, width))
    edgeItems.push(...headPaths(startHead, transform(tipStart), axis(-1), paint, width))

    const labelNode = chain.find(node => node.kind === 'label')
    if (edge.label) {
      const middle = points[Math.floor(points.length / 2)]!
      const before = points[Math.floor(points.length / 2) - 1] ?? middle
      const at: [number, number] = labelNode ? [labelNode.x, labelNode.y] : [(middle[0] + before[0]) / 2, (middle[1] + before[1]) / 2]
      labelItems.push(...edgeLabel(edge.label, transform(at)))
    }
  }

  // Links between nodes of one rank run straight across it.
  for (const edge of flats) {
    const { edge: source } = edge
    if (source.line === 'invisible')
      continue
    const [a, b] = [nodes.get(edge.u)!, nodes.get(edge.v)!].sort((left, right) => left.x - right.x) as [LNode, LNode]
    const forward = a.key === (edge.reversed ? edge.v : edge.u)
    const startHead = forward ? source.start : source.end
    const endHead = forward ? source.end : source.start
    const tipStart: [number, number] = [a.x + a.w / 2 + a.extra, a.y]
    const tipEnd: [number, number] = [b.x - b.w / 2, b.y]
    const from: [number, number] = [tipStart[0] + headInset(startHead), a.y]
    const to: [number, number] = [tipEnd[0] - headInset(endHead), b.y]
    const third = (to[0] - from[0]) / 3
    const paint = edgePaint(source)
    const width = edgeWidth(source)
    const [x0, y0] = transform([0, 0])
    const [x1, y1] = transform([1, 0])
    const across: [number, number] = [x1 - x0, y1 - y0]
    edgeItems.push({
      kind: 'path',
      d: [['M', ...transform(from)], ['C', ...transform([from[0] + third, from[1]]), ...transform([to[0] - third, to[1]]), ...transform(to)]],
      stroke: paint,
      width,
      ...(source.line === 'dotted' || source.style?.dashed ? { dash: [4, 4] } : {}),
    })
    edgeItems.push(...headPaths(endHead, transform(tipEnd), across, paint, width))
    edgeItems.push(...headPaths(startHead, transform(tipStart), [-across[0], -across[1]], paint, width))
    if (edge.label)
      labelItems.push(...edgeLabel(edge.label, transform([(from[0] + to[0]) / 2, a.y - edge.label.height / 2 - 6])))
  }

  for (const loop of loops) {
    const { node, edge } = loop
    const right = node.x + node.w / 2
    const reach = loop.reach
    const bend = (24 + Math.min(node.h, 40) / 2) * 0.7
    const offset = Math.min(node.h / 4, 10)
    const top = node.y - offset
    const bottom = node.y + offset
    const sides = outlineExtents(outlineFor(node), graph.direction, 'x', 1, [-offset, offset])
    const startX = node.x + sides[0]!
    const endX = node.x + sides[1]!
    const inset = headInset(edge.end)
    const d: PathCommand[] = [
      ['M', ...transform([startX + headInset(edge.start), top])],
      ['C', ...transform([right + reach, top - bend]), ...transform([right + reach, bottom + bend]), ...transform([endX + inset, bottom])],
    ]
    const paint = edgePaint(edge)
    const width = edgeWidth(edge)
    if (edge.line !== 'invisible') {
      edgeItems.push({
        kind: 'path',
        d,
        stroke: paint,
        width,
        ...(edge.line === 'dotted' || edge.style?.dashed ? { dash: [4, 4] } : {}),
      })
      const [x0, y0] = transform([0, 0])
      const [x1, y1] = transform([-1, 0])
      edgeItems.push(...headPaths(edge.end, transform([endX, bottom]), [x1 - x0, y1 - y0], paint, width))
      edgeItems.push(...headPaths(edge.start, transform([startX, top]), [x1 - x0, y1 - y0], paint, width))
      if (loop.label) {
        const at = transform([right + reach * 0.75 + (horizontal ? loop.label.height : loop.label.width) / 2 + 6, node.y])
        labelItems.push(...edgeLabel(loop.label, at))
      }
    }
  }

  for (const node of nodes.values()) {
    if (!node.node || !node.box)
      continue
    const source = node.node
    const [cx, cy] = transform([node.x, node.y])
    const { w, h } = node.box
    const tone = family(source.shape)
    const style = source.style
    const solid = source.shape === 'start' || source.shape === 'fork'
    const { outlines, details } = shapePaths(source.shape, cx, cy, w, h)
    const fill: Paint = style?.fill ? { color: style.fill } : solid ? { tone: 'ink' } : { tone }
    const stroke: Paint = style?.stroke ? { color: style.stroke } : solid ? { tone: 'ink' } : { tone: `${tone}-line` }
    for (const d of outlines) {
      nodeItems.push({
        kind: 'path',
        d,
        fill,
        stroke,
        width: style?.strokeWidth ?? (source.shape === 'end' ? 1.5 : 1.25),
        ...(style?.dashed ? { dash: [5, 4] } : {}),
        ...(source.link ? { link: source.link } : {}),
        ...(solid || source.shape === 'end' ? {} : { shadow: true }),
      })
    }

    for (const d of details) {
      const filled = source.shape === 'end'
      nodeItems.push({
        kind: 'path',
        d,
        ...(filled ? { fill: { tone: 'ink' } as Paint } : {}),
        stroke: filled ? { tone: 'ink' } : stroke,
        width: 1.25,
        ...(source.link ? { link: source.link } : {}),
      })
    }

    if (node.box.text.lines.length && source.shape !== 'start' && source.shape !== 'end' && source.shape !== 'fork' && source.shape !== 'choice') {
      nodeItems.push({
        kind: 'text',
        x: cx,
        y: cy + (source.shape === 'cylinder' ? 5 : 0),
        lines: node.box.text.lines,
        size: NODE_FONT,
        weight: 500,
        anchor: 'middle',
        fill: textOn(style, `${tone}-ink`),
        ...(source.link ? { link: source.link } : {}),
      })
    }
  }

  items.push(...backgrounds, ...edgeItems, ...titleItems, ...labelItems, ...nodeItems)
  const count = graph.nodes.length
  return fitScene({
    width,
    height,
    items,
    label: graph.title ?? `Flowchart with ${count} ${count === 1 ? 'node' : 'nodes'}`,
  }, measure)
}

/**
 * A smooth curve through layout points. Ends leave along the rank axis; interior points take
 * Catmull-Rom tangents, so collinear runs stay straight.
 */
function smoothPath(points: [number, number][], transform: (point: [number, number]) => [number, number]): PathCommand[] {
  const tangents = points.map((point, index): [number, number] => {
    if (index === 0)
      return [0, (points[1]![1] - point[1]) * 1.5]
    if (index === points.length - 1)
      return [0, (point[1] - points[index - 1]![1]) * 1.5]
    const previous = points[index - 1]!
    const next = points[index + 1]!
    return [(next[0] - previous[0]) / 2, (next[1] - previous[1]) / 2]
  })
  const d: PathCommand[] = [['M', ...transform(points[0]!)]]
  for (let index = 1; index < points.length; index++) {
    const [ax, ay] = points[index - 1]!
    const [bx, by] = points[index]!
    const [mx, my] = tangents[index - 1]!
    const [nx, ny] = tangents[index]!
    d.push(['C', ...transform([ax + mx / 3, ay + my / 3]), ...transform([bx - nx / 3, by - ny / 3]), ...transform([bx, by])])
  }

  return d
}

function edgeLabel(label: TextBlock, [x, y]: [number, number]): SceneItem[] {
  const w = label.width + 12
  const h = label.height + 4
  return [
    { kind: 'path', d: roundedRect(x - w / 2, y - h / 2, w, h, 6), fill: { tone: 'label' } },
    {
      kind: 'text',
      x,
      y,
      lines: label.lines,
      size: LABEL_FONT,
      weight: 500,
      anchor: 'middle',
      fill: { tone: 'muted' },
    },
  ]
}
