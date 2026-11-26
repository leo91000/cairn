package build.cairn.app.ui

import kotlin.math.abs
import kotlin.math.ceil
import kotlin.math.max
import kotlin.math.min

// ---------------------------------------------------------------------------
// Layered graph layout (flowcharts and state diagrams).

private enum class LKind {
    NODE,
    DUMMY,
    LABEL,
    LEFT,
    RIGHT,
}

private class LNode(
    val key: String,
    val kind: LKind,
    val w: Double,
    val h: Double,
    /** Innermost containing cluster. */
    val cluster: String?,
    var rank: Int = 0,
    val node: GraphNode? = null,
    val box: NodeBox? = null,
) {
    var order = 0
    var x = 0.0
    var y = 0.0
    /** Extra room on the right for self loops. */
    var extra = 0.0
    /** Extra space along the rank axis for tall/wide loop labels, without moving shape ports. */
    var rankPadding = 0.0
}

private class LEdge(
    val edge: GraphEdge,
    val index: Int,
    val u: String,
    val v: String,
    val reversed: Boolean,
    val label: TextBlock?,
) {
    var chain: List<LNode> = emptyList()
}

private class Box(val x: Double, val y: Double, val w: Double, val h: Double)

private class Constraint(var u: String, var v: String, val minlen: Int)

private class Link(val edge: GraphEdge, val index: Int, val label: TextBlock?) {
    var back = false
}

private class SelfLoop(
    val edge: GraphEdge,
    val node: LNode,
    val label: TextBlock?,
    val reach: Double,
)

private class Neighbors {
    val up = mutableListOf<LNode>()
    val down = mutableListOf<LNode>()
}

private class Borders {
    val left = mutableListOf<LNode>()
    val right = mutableListOf<LNode>()
}

private class Entry(val key: Double, val nodes: List<LNode>, val cluster: String? = null)

private class Block(var weight: Double, var total: Double, var count: Int)

private fun <K, V> MutableMap<K, MutableList<V>>.append(key: K, value: V) {
    getOrPut(key) { mutableListOf() }.add(value)
}

/** Iterative depth-first search: links reaching a node still on the stack are back links. */
private fun <T> backLinks(
    starts: Iterable<String>,
    from: Map<String, List<T>>,
    target: (T) -> String,
): List<T> {
    val state = HashMap<String, Int>()
    val back = mutableListOf<T>()
    for (start in starts) {
        if (state.containsKey(start)) continue
        val stack = mutableListOf(start to IntArray(1))
        state[start] = 1
        while (stack.isNotEmpty()) {
            val (key, position) = stack.last()
            val list = from[key].orEmpty()
            if (position[0] >= list.size) {
                state[key] = 2
                stack.removeAt(stack.size - 1)
                continue
            }

            val link = list[position[0]++]
            val next = target(link)
            val seen = state[next]
            if (seen == 1) {
                back.add(link)
            } else if (seen == null) {
                state[next] = 1
                stack.add(next to IntArray(1))
            }
        }
    }

    return back
}

internal fun layoutGraph(graph: GraphDiagram, measure: TextMeasure): Scene {
    if (graph.nodes.isEmpty() && graph.clusters.isEmpty())
        throw DiagramError("This diagram has nothing to draw.")
    val horizontal = graph.direction == Direction.LR || graph.direction == Direction.RL
    val annotations = LinkedHashMap<String, MutableList<GraphNode>>()
    val annotationBoxes = HashMap<String, NodeBox>()
    for (note in graph.nodes) {
        val annotation = note.annotation ?: continue
        val target = graph.nodes.find { it.id == annotation.target && it.annotation == null }
        if (target == null || graph.edges.any { it.from == target.id && it.to == target.id })
            throw DiagramError(
                "Notes on composite states or states with self transitions are not supported."
            )
        annotations.getOrPut(target.id) { mutableListOf() }.add(note)
        annotationBoxes[note.id] = nodeBox(note, horizontal, measure)
    }
    val clusters = LinkedHashMap<String, GraphCluster>()
    for (cluster in graph.clusters) clusters[cluster.id] = cluster
    val chains = HashMap<String, List<String>>()
    fun chainOf(id: String?): List<String> {
        if (id.isNullOrEmpty()) return emptyList()
        return chains.getOrPut(id) {
            val chain = ArrayDeque<String>()
            var current: String? = id
            while (!current.isNullOrEmpty() && clusters.containsKey(current)) {
                chain.addFirst(current)
                current = clusters.getValue(current).parent
            }
            chain.toList()
        }
    }

    // Layout nodes, in layout space: x across ranks, y along them.
    val nodes = LinkedHashMap<String, LNode>()
    fun add(node: LNode): LNode {
        if (!nodes.containsKey(node.key) && nodes.size >= MAX_LAYOUT_NODES)
            throw DiagramError("This diagram is too large to draw.")
        nodes[node.key] = node
        return node
    }

    for (node in graph.nodes) {
        if (node.annotation != null) continue
        val box = nodeBox(node, horizontal, measure)
        val notes = annotations[node.id].orEmpty()
        fun sideHeight(side: NotePlacement) =
            notes
                .filter { it.annotation!!.side == side }
                .sumOf { annotationBoxes.getValue(it.id).h + 12 } - 12
        val noteWidth = notes.maxOfOrNull { annotationBoxes.getValue(it.id).w + 28 } ?: 0.0
        val reservedWidth = box.w + noteWidth * 2
        val reservedHeight =
            max(box.h, max(sideHeight(NotePlacement.LEFT), sideHeight(NotePlacement.RIGHT)))
        add(
            LNode(
                key = node.id,
                kind = LKind.NODE,
                w = if (horizontal) reservedHeight else reservedWidth,
                h = if (horizontal) reservedWidth else reservedHeight,
                cluster = node.parent,
                node = node,
                box = box,
            )
        )
    }

    val titles = HashMap<String, TextBlock>()
    for (cluster in graph.clusters) titles[cluster.id] =
        textBlock(cluster.label, CLUSTER_FONT, 600, 240.0, measure)

    // An empty cluster still draws its box around a placeholder.
    val members = LinkedHashMap<String, MutableList<String>>()
    for (node in graph.nodes) {
        if (node.annotation != null) continue
        for (id in chainOf(node.parent)) members.append(id, node.id)
    }

    for (cluster in graph.clusters) {
        if (members.containsKey(cluster.id)) continue
        val key = "\u0000empty:${cluster.id}"
        val title = titles.getValue(cluster.id)
        add(
            LNode(
                key = key,
                kind = LKind.DUMMY,
                w = if (horizontal) 24.0 else max(title.width, 48.0),
                h = 24.0,
                cluster = cluster.id,
            )
        )
        for (id in chainOf(cluster.id)) members.append(id, key)
    }

    // Rank constraints. A link to or from a subgraph constrains every member, so the whole
    // subgraph sits before or after the other end, as its box will be drawn.
    val constraints = mutableListOf<Constraint>()
    fun ends(id: String): List<String> =
        if (clusters.containsKey(id)) members.getValue(id) else listOf(id)
    val links = mutableListOf<Link>()
    val loops = mutableListOf<SelfLoop>()
    val nextLoopReach = mutableMapOf<String, Double>()
    graph.edges.forEachIndexed { index, edge ->
        val label =
            if (edge.label.isNotEmpty()) textBlock(edge.label, LABEL_FONT, 500, 160.0, measure)
            else null
        if (edge.from == edge.to) {
            val node = nodes[edge.from]
            if (node != null) {
                val reach = nextLoopReach[node.key] ?: (24 + min(node.h, 40.0) / 2)
                val labelExtent =
                    if (label != null) (if (horizontal) label.height else label.width) else 0.0
                val rankExtent =
                    if (label != null) (if (horizontal) label.width else label.height) else 0.0
                nextLoopReach[node.key] = reach + max(32.0, (labelExtent + 16) / 0.75)
                node.extra = max(node.extra, max(reach + 12, reach * 0.75 + labelExtent + 18))
                node.rankPadding = max(node.rankPadding, (rankExtent - node.h) / 2 + 6)
                loops.add(SelfLoop(edge, node, label, reach))
            }

            return@forEachIndexed
        }

        links.add(Link(edge, index, label))
    }

    // Break cycles twice: first between links as written, with subgraphs as single ends, so a
    // link back into a subgraph is the one reversed; then between the members it expands to.
    val linksFrom = LinkedHashMap<String, MutableList<Link>>()
    for (link in links) linksFrom.append(link.edge.from, link)
    for (link in
        backLinks(graph.nodes.map { it.id } + clusters.keys, linksFrom) { it.edge.to }) link.back =
        true

    for (link in links) {
        val from = if (link.back) link.edge.to else link.edge.from
        val to = if (link.back) link.edge.from else link.edge.to
        for (u in ends(from)) {
            for (v in ends(to)) {
                if (u != v) {
                    if (constraints.size >= MAX_LAYOUT_CONSTRAINTS)
                        throw DiagramError("This diagram has too many expanded links to draw.")
                    constraints.add(Constraint(u, v, link.edge.length * 2))
                }
            }
        }
    }

    val outgoing = LinkedHashMap<String, MutableList<Constraint>>()
    for (constraint in constraints) outgoing.append(constraint.u, constraint)
    val backward = LinkedHashSet(backLinks(nodes.keys.toList(), outgoing) { it.v })
    for (constraint in backward) {
        val u = constraint.u
        constraint.u = constraint.v
        constraint.v = u
    }

    // Rank by longest path, then pull nodes with more outgoing than incoming constraints down.
    val ins = HashMap<String, MutableList<Constraint>>()
    val outs = HashMap<String, MutableList<Constraint>>()
    for (constraint in constraints) {
        ins.append(constraint.v, constraint)
        outs.append(constraint.u, constraint)
    }

    val indegree = HashMap<String, Int>()
    for (key in nodes.keys) indegree[key] = ins[key]?.size ?: 0
    val queue = ArrayDeque(nodes.keys.filter { indegree[it] == 0 })
    val topo = mutableListOf<String>()
    while (queue.isNotEmpty()) {
        val key = queue.removeFirst()
        topo.add(key)
        for (constraint in outs[key].orEmpty()) {
            val next = nodes.getValue(constraint.v)
            next.rank = max(next.rank, nodes.getValue(key).rank + constraint.minlen)
            indegree[constraint.v] = indegree.getValue(constraint.v) - 1
            if (indegree[constraint.v] == 0) queue.addLast(constraint.v)
        }
    }

    for (key in topo.asReversed()) {
        val out = outs[key].orEmpty()
        if (out.isEmpty() || out.size <= (ins[key]?.size ?: 0)) continue
        val node = nodes.getValue(key)
        node.rank = out.minOf { nodes.getValue(it.v).rank - it.minlen }
    }

    val lowest = nodes.values.minOfOrNull { it.rank } ?: 0
    for (node in nodes.values) node.rank -= lowest

    // With ranks known, a subgraph's link attaches to its member nearest the other end.
    fun nearest(id: String, rank: Double): String {
        if (!clusters.containsKey(id)) return id
        var best = members.getValue(id)[0]
        for (key in members.getValue(id)) {
            if (abs(nodes.getValue(key).rank - rank) < abs(nodes.getValue(best).rank - rank))
                best = key
        }

        return best
    }

    val edges = mutableListOf<LEdge>()
    val flats = mutableListOf<LEdge>()
    for (link in links) {
        val edge = link.edge
        fun middle(id: String): Double {
            val ranks = ends(id).map { nodes.getValue(it).rank }
            return ranks.fold(0.0) { sum, rank -> sum + rank } / ranks.size
        }

        val u = nearest(edge.from, middle(edge.to))
        val v = nearest(edge.to, nodes.getValue(u).rank.toDouble())
        if (u == v) continue
        val reversed = nodes.getValue(u).rank > nodes.getValue(v).rank
        val layout =
            LEdge(
                edge = edge,
                index = link.index,
                u = if (reversed) v else u,
                v = if (reversed) u else v,
                reversed = reversed,
                label = link.label,
            )
        if (nodes.getValue(u).rank == nodes.getValue(v).rank) flats.add(layout)
        else edges.add(layout)
    }

    // Each edge becomes a chain of dummies, one per intermediate rank; labels take the middle one.
    fun lca(a: String?, b: String?): String? {
        val left = chainOf(a)
        val right = chainOf(b)
        var common: String? = null
        var index = 0
        while (index < min(left.size, right.size) && left[index] == right[index]) {
            common = left[index]
            index++
        }
        return common
    }

    for (edge in edges) {
        val u = nodes.getValue(edge.u)
        val v = nodes.getValue(edge.v)
        val cluster = lca(u.cluster, v.cluster)
        val middle = u.rank + (v.rank - u.rank) / 2
        val chain = mutableListOf(u)
        for (rank in u.rank + 1 until v.rank) {
            val label = edge.label
            val labelled = label != null && rank == middle
            chain.add(
                add(
                    LNode(
                        key = "\u0000edge:${edge.index}:$rank",
                        kind = if (labelled) LKind.LABEL else LKind.DUMMY,
                        w =
                            if (labelled) (if (horizontal) label.height + 8 else label.width + 14)
                            else 0.0,
                        h =
                            if (labelled) (if (horizontal) label.width + 14 else label.height + 8)
                            else 0.0,
                        cluster = cluster,
                        rank = rank,
                    )
                )
            )
        }

        chain.add(v)
        edge.chain = chain
    }

    // Clusters keep a left and right border node on every rank they span.
    val spans = LinkedHashMap<String, IntArray>()
    for (node in nodes.values) {
        for (id in chainOf(node.cluster)) {
            val span = spans[id]
            spans[id] =
                if (span != null) intArrayOf(min(span[0], node.rank), max(span[1], node.rank))
                else intArrayOf(node.rank, node.rank)
        }
    }

    for ((id, span) in spans) {
        for (rank in span[0]..span[1]) {
            for (kind in listOf(LKind.LEFT, LKind.RIGHT)) {
                val name = if (kind == LKind.LEFT) "left" else "right"
                add(
                    LNode(
                        key = "\u0000$name:$id:$rank",
                        kind = kind,
                        w = 0.0,
                        h = 0.0,
                        cluster = id,
                        rank = rank,
                    )
                )
            }
        }
    }

    val rankCount = max(0, nodes.values.maxOfOrNull { it.rank } ?: 0) + 1
    var layers: MutableList<List<LNode>> = MutableList(rankCount) { emptyList() }
    val building = List(rankCount) { mutableListOf<LNode>() }
    val neighbors = HashMap<String, Neighbors>()
    for (node in nodes.values) neighbors[node.key] = Neighbors()
    for (edge in edges) {
        for (index in 1 until edge.chain.size) {
            neighbors.getValue(edge.chain[index - 1].key).down.add(edge.chain[index])
            neighbors.getValue(edge.chain[index].key).up.add(edge.chain[index - 1])
        }
    }

    // Initial order: depth-first from the declared nodes, so authored order mostly survives.
    val visited = HashSet<String>()
    fun visit(start: LNode) {
        val stack = mutableListOf(start)
        while (stack.isNotEmpty()) {
            val node = stack.removeAt(stack.size - 1)
            if (!visited.add(node.key)) continue
            building[node.rank].add(node)
            stack.addAll(neighbors.getValue(node.key).down.asReversed())
        }
    }

    for (node in nodes.values) {
        if (node.kind == LKind.NODE || node.kind == LKind.DUMMY) visit(node)
    }

    for (node in nodes.values) {
        if (!visited.contains(node.key)) visit(node)
    }

    for (rank in 0 until rankCount) layers[rank] = building[rank]

    fun clusterDepth(id: String?) = chainOf(id).size
    val siblingOrder = HashMap<String, Double>()

    /** Sort one layer by key, keeping each cluster contiguous and bordered. */
    fun arrange(layer: List<LNode>, key: (LNode) -> Double): List<LNode> {
        fun place(items: List<LNode>, depth: Int, inside: String?): List<LNode> {
            val entries = mutableListOf<Entry>()
            val groups = LinkedHashMap<String, MutableList<LNode>>()
            var left: LNode? = null
            var right: LNode? = null
            for (node in items) {
                val chain = chainOf(node.cluster)
                if (chain.size == depth) {
                    if (node.kind == LKind.LEFT && node.cluster == inside) left = node
                    else if (node.kind == LKind.RIGHT && node.cluster == inside) right = node
                    else entries.add(Entry(key(node), listOf(node)))
                } else {
                    groups.append(chain[depth], node)
                }
            }

            for ((cluster, group) in groups) {
                val keyed = group.filter { it.kind != LKind.LEFT && it.kind != LKind.RIGHT }
                val value =
                    if (keyed.isNotEmpty())
                        keyed.fold(0.0) { sum, node -> sum + key(node) } / keyed.size
                    else key(group[0])
                entries.add(Entry(value, place(group, depth + 1, cluster), cluster))
            }

            entries.sortWith { a, b -> sortOrder(a.key - b.key) }
            // Sibling clusters keep one relative order on every rank, or their boxes would overlap.
            val slots = entries.indices.filter { entries[it].cluster != null }
            val ordered =
                slots
                    .map { entries[it] }
                    .sortedWith { a, b ->
                        sortOrder(
                            (siblingOrder[a.cluster!!] ?: 0.0) - (siblingOrder[b.cluster!!] ?: 0.0)
                        )
                    }
            slots.forEachIndexed { index, slot -> entries[slot] = ordered[index] }
            return listOfNotNull(left) + entries.flatMap { it.nodes } + listOfNotNull(right)
        }

        val sorted = place(layer, 0, null)
        sorted.forEachIndexed { index, node -> node.order = index }
        return sorted
    }

    fun updateSiblingOrder() {
        val totals = LinkedHashMap<String, DoubleArray>()
        for (layer in layers) {
            for (node in layer) {
                if (node.cluster.isNullOrEmpty()) continue
                for (id in chainOf(node.cluster)) {
                    val total = totals[id] ?: doubleArrayOf(0.0, 0.0)
                    totals[id] =
                        doubleArrayOf(
                            total[0] + node.order.toDouble() / max(1, layer.size - 1),
                            total[1] + 1,
                        )
                }
            }
        }

        for ((id, total) in totals) siblingOrder[id] = total[0] / total[1]
    }

    // Crossings between two layers are inversions of the lower ends, counted with a Fenwick tree.
    fun crossings(): Int {
        var count = 0
        for (rank in 0 until layers.size - 1) {
            val ends = mutableListOf<Int>()
            for (node in layers[rank]) {
                ends.addAll(neighbors.getValue(node.key).down.map { it.order }.sorted())
            }

            val size = layers[rank + 1].size + 1
            val tree = IntArray(size + 1)
            for ((index, end) in ends.withIndex()) {
                var below = 0
                var at = end + 1
                while (at > 0) {
                    below += tree[at]
                    at -= at and -at
                }
                count += index - below
                at = end + 1
                while (at <= size) {
                    tree[at]++
                    at += at and -at
                }
            }
        }

        return count
    }

    for (layer in layers) layer.forEachIndexed { index, node -> node.order = index }

    updateSiblingOrder()
    for (rank in layers.indices) layers[rank] = arrange(layers[rank]) { it.order.toDouble() }

    var best = layers.map { it.toList() }
    var bestCrossings = crossings()
    fun barycenter(node: LNode, up: Boolean): Double {
        val list = neighbors.getValue(node.key).let { if (up) it.up else it.down }
        if (list.isEmpty()) return node.order.toDouble()
        return list.fold(0.0) { sum, next -> sum + next.order } / list.size
    }

    var iteration = 0
    while (iteration < 16 && bestCrossings > 0) {
        val down = iteration % 2 == 0
        updateSiblingOrder()
        val ranks = layers.indices.toList()
        for (rank in if (down) ranks.drop(1) else ranks.asReversed().drop(1)) layers[rank] =
            arrange(layers[rank]) { barycenter(it, up = down) }
        val count = crossings()
        if (count < bestCrossings) {
            bestCrossings = count
            best = layers.map { it.toList() }
        }
        iteration++
    }

    for ((rank, layer) in best.withIndex()) {
        layers[rank] = layer
        layer.forEachIndexed { index, node -> node.order = index }
    }

    // Horizontal positions. Separation depends on what sits side by side.
    fun separation(a: LNode, b: LNode): Double {
        val half = (a.w + b.w) / 2 + a.extra
        if (a.kind == LKind.LEFT && b.cluster == a.cluster && b.kind != LKind.RIGHT)
            return half +
                CLUSTER_PAD +
                (if (horizontal) titles.getValue(a.cluster!!).height + 6 else 0.0)
        if (a.kind == LKind.LEFT || b.kind == LKind.RIGHT) return half + CLUSTER_PAD
        if (a.kind == LKind.RIGHT || b.kind == LKind.LEFT)
            return half + (if (a.kind == LKind.RIGHT && b.kind == LKind.LEFT) 14 else 20)
        if (a.kind == LKind.DUMMY || b.kind == LKind.DUMMY) return half + EDGE_SEP
        return half + NODE_SEP
    }

    fun weight(a: LNode, b: LNode): Double {
        val dummyA = a.kind == LKind.DUMMY || a.kind == LKind.LABEL
        val dummyB = b.kind == LKind.DUMMY || b.kind == LKind.LABEL
        return if (dummyA && dummyB) 8.0 else if (dummyA || dummyB) 2.0 else 1.0
    }

    for (layer in layers) {
        for (index in 1 until layer.size) {
            val least = layer[index - 1].x + separation(layer[index - 1], layer[index])
            if (layer[index].x < least) layer[index].x = least
        }
    }

    /** Pool-adjacent-violators: closest positions to the targets that keep the separations. */
    fun solve(layer: List<LNode>, targets: List<Double>, weights: List<Double>) {
        val offsets = mutableListOf(0.0)
        for (index in 1 until layer.size) offsets.add(
            offsets[index - 1] + separation(layer[index - 1], layer[index])
        )
        val blocks = mutableListOf<Block>()
        for (index in layer.indices) {
            blocks.add(Block(weights[index], weights[index] * (targets[index] - offsets[index]), 1))
            while (blocks.size > 1) {
                val last = blocks[blocks.size - 1]
                val previous = blocks[blocks.size - 2]
                if (previous.total / previous.weight <= last.total / last.weight) break
                blocks.removeAt(blocks.size - 1)
                previous.weight += last.weight
                previous.total += last.total
                previous.count += last.count
            }
        }

        var index = 0
        for (block in blocks) {
            val value = block.total / block.weight
            repeat(block.count) {
                layer[index].x = value + offsets[index]
                index++
            }
        }
    }

    for (pass in 0 until 10) {
        val down = pass % 2 == 0
        val ranks = layers.indices.toList()
        for (rank in if (down) ranks else ranks.asReversed()) {
            val layer = layers[rank]
            val targets = mutableListOf<Double>()
            val weights = mutableListOf<Double>()
            for (node in layer) {
                val around = neighbors.getValue(node.key)
                val primary = if (down) around.up else around.down
                val list =
                    if (primary.isNotEmpty()) primary else if (down) around.down else around.up
                if (list.isEmpty()) {
                    targets.add(node.x)
                    weights.add(
                        if (node.kind == LKind.LEFT || node.kind == LKind.RIGHT) 0.25 else 0.5
                    )
                    continue
                }

                var total = 0.0
                var sum = 0.0
                for (next in list) {
                    val w = weight(node, next)
                    total += w
                    sum += w * next.x
                }

                targets.add(sum / total)
                weights.add(total)
            }

            solve(layer, targets, weights)
        }
    }

    // A border has one position across every rank. Solve their shared separation constraints
    // together so aligning one group cannot undo another rank's clearance.
    val borders = LinkedHashMap<String, Borders>()
    for (node in nodes.values) {
        if (node.kind != LKind.LEFT && node.kind != LKind.RIGHT) continue
        val entry = borders.getOrPut(node.cluster!!) { Borders() }
        if (node.kind == LKind.LEFT) entry.left.add(node) else entry.right.add(node)
    }

    fun minimumWidth(id: String) =
        if (horizontal) 0.0 else titles.getValue(id).width + CLUSTER_PAD * 2
    fun representative(node: LNode): LNode =
        when (node.kind) {
            LKind.LEFT -> borders.getValue(node.cluster!!).left.first()
            LKind.RIGHT -> borders.getValue(node.cluster!!).right.first()
            else -> node
        }

    val borderPositions = LinkedHashMap<LNode, Double>()
    val borderIncoming = LinkedHashMap<LNode, Int>()
    val borderOutgoing = LinkedHashMap<LNode, MutableList<Pair<LNode, Double>>>()
    for (node in nodes.values) {
        val key = representative(node)
        val existing = borderPositions[key]
        borderPositions[key] =
            when {
                existing == null -> node.x
                node.kind == LKind.LEFT -> min(existing, node.x)
                else -> max(existing, node.x)
            }
        borderIncoming[key] = 0
        borderOutgoing[key] = mutableListOf()
    }

    fun constrain(left: LNode, right: LNode, distance: Double) {
        val from = representative(left)
        val to = representative(right)
        borderOutgoing.getValue(from).add(Pair(to, distance))
        borderIncoming[to] = borderIncoming.getValue(to) + 1
    }

    for (layer in layers) {
        for (index in 1 until layer.size) constrain(
            layer[index - 1],
            layer[index],
            separation(layer[index - 1], layer[index]),
        )
    }
    for ((id, entry) in borders) constrain(
        entry.left.first(),
        entry.right.first(),
        minimumWidth(id),
    )

    val borderReady =
        borderPositions.keys.filter { borderIncoming.getValue(it) == 0 }.toMutableList()
    var borderIndex = 0
    while (borderIndex < borderReady.size) {
        val from = borderReady[borderIndex++]
        for ((node, distance) in borderOutgoing.getValue(from)) {
            borderPositions[node] =
                max(borderPositions.getValue(node), borderPositions.getValue(from) + distance)
            val remaining = borderIncoming.getValue(node) - 1
            borderIncoming[node] = remaining
            if (remaining == 0) borderReady.add(node)
        }
    }
    if (borderReady.size != borderPositions.size)
        throw DiagramError("These groups cannot be separated without overlap.")
    for (node in nodes.values) node.x = borderPositions.getValue(representative(node))

    // Straighten long edges where their layers leave room for a direct line.
    fun fits(node: LNode, x: Double): Boolean {
        val layer = layers[node.rank]
        val left = layer.getOrNull(node.order - 1)
        val right = layer.getOrNull(node.order + 1)
        return (left == null || left.x + separation(left, node) <= x + 0.01) &&
            (right == null || x + separation(node, right) <= right.x + 0.01)
    }

    repeat(2) {
        for (edge in edges) {
            val inner = edge.chain.subList(1, edge.chain.size - 1)
            if (inner.isEmpty()) continue
            val a = edge.chain[0]
            val b = edge.chain.last()
            fun target(node: LNode) = a.x + (b.x - a.x) * (node.rank - a.rank) / (b.rank - a.rank)
            if (inner.all { fits(it, target(it)) }) {
                for (node in inner) node.x = target(node)
            }
        }
    }

    // Center each subgraph's content when its title made the box wider.
    val deepestFirst = graph.clusters.sortedWith { a, b -> clusterDepth(b.id) - clusterDepth(a.id) }
    for (cluster in deepestFirst) {
        val entry = borders[cluster.id] ?: continue
        val inside =
            nodes.values.filter {
                !it.cluster.isNullOrEmpty() &&
                    chainOf(it.cluster).contains(cluster.id) &&
                    !((it.kind == LKind.LEFT || it.kind == LKind.RIGHT) && it.cluster == cluster.id)
            }
        if (inside.isEmpty()) continue
        val contentLeft = minimum(inside.map { it.x - it.w / 2 })
        val contentRight = maximum(inside.map { it.x + it.w / 2 + it.extra })
        val leftRoom =
            contentLeft -
                entry.left[0].x -
                CLUSTER_PAD -
                (if (horizontal) titles.getValue(cluster.id).height + 6 else 0.0)
        val rightRoom = entry.right[0].x - contentRight - CLUSTER_PAD
        var shift = (rightRoom - leftRoom) / 2
        val moving = inside.toSet()
        // Every rank keeps its separation constraints when the whole cluster moves together.
        for (node in inside) {
            val right = layers[node.rank].getOrNull(node.order + 1)
            if (right != null && right !in moving)
                shift = min(shift, right.x - node.x - separation(node, right))
        }
        if (shift > 0.5) {
            for (node in inside) node.x += shift
        }
    }

    // Horizontal titles need space along the rank axis, including in empty/nested clusters.
    if (horizontal) {
        for (node in nodes.values) {
            if (node.kind == LKind.LEFT || node.kind == LKind.RIGHT) continue
            for (id in chainOf(node.cluster)) {
                if (spans.getValue(id)[0] == node.rank)
                    node.rankPadding =
                        max(node.rankPadding, (titles.getValue(id).width - node.h) / 2)
            }
        }
    }

    // Vertical positions: rank gaps grow where clusters open or close.
    val rankHeight = layers.map { layer ->
        layer.fold(0.0) { tallest, node -> max(tallest, node.h + node.rankPadding * 2) }
    }
    val opening = DoubleArray(rankCount)
    val closing = DoubleArray(rankCount)
    for (layer in layers) {
        for (node in layer) {
            var opens = 0.0
            var closes = 0.0
            for (id in chainOf(node.cluster)) {
                val span = spans.getValue(id)
                if (span[0] == node.rank)
                    opens +=
                        CLUSTER_PAD +
                            (if (graph.direction == Direction.TB) titles.getValue(id).height + 4
                            else 0.0)
                if (span[1] == node.rank)
                    closes +=
                        CLUSTER_PAD +
                            (if (graph.direction == Direction.BT) titles.getValue(id).height + 4
                            else 0.0)
            }

            opening[node.rank] = max(opening[node.rank], opens)
            closing[node.rank] = max(closing[node.rank], closes)
        }
    }

    val rankY = DoubleArray(rankCount)
    var cursor = 0.0
    for (rank in 0 until rankCount) {
        cursor += (if (rank != 0) RANK_GAP else 0.0) + opening[rank]
        rankY[rank] = cursor + rankHeight[rank] / 2
        cursor += rankHeight[rank] + closing[rank]
    }

    for (node in nodes.values) node.y = rankY[node.rank]

    // Notes reserve space with their target, then sit on the requested physical side in every
    // direction.
    for ((target, notes) in annotations) {
        val node = nodes.getValue(target)
        for (side in listOf(NotePlacement.LEFT, NotePlacement.RIGHT)) {
            val attached = notes.filter { it.annotation!!.side == side }
            val total = attached.sumOf { annotationBoxes.getValue(it.id).h + 12 } - 12
            var y = -total / 2
            for (note in attached) {
                val box = annotationBoxes.getValue(note.id)
                val dx =
                    (if (side == NotePlacement.LEFT) -1 else 1) *
                        (node.box!!.w / 2 + 28 + box.w / 2)
                val dy = y + box.h / 2
                val offset =
                    when (graph.direction) {
                        Direction.BT -> Pt(dx, -dy)
                        Direction.LR -> Pt(dy, dx)
                        Direction.RL -> Pt(dy, -dx)
                        else -> Pt(dx, dy)
                    }
                val noteNode =
                    add(
                        LNode(
                            key = note.id,
                            kind = LKind.NODE,
                            w = if (horizontal) box.h else box.w,
                            h = if (horizontal) box.w else box.h,
                            cluster = node.cluster,
                            rank = node.rank,
                            node = note,
                            box = box,
                        )
                    )
                noteNode.x = node.x + offset.x
                noteNode.y = node.y + offset.y
                y += box.h + 12
            }
        }
    }

    // Cluster boxes, innermost first so parents can include their children.
    val boxes = LinkedHashMap<String, Box>()
    for (cluster in deepestFirst) {
        if (!spans.containsKey(cluster.id)) continue
        val entry = borders.getValue(cluster.id)
        var top = Double.POSITIVE_INFINITY
        var bottom = Double.NEGATIVE_INFINITY
        for (node in nodes.values) {
            if (
                node.kind == LKind.LEFT ||
                    node.kind == LKind.RIGHT ||
                    !chainOf(node.cluster).contains(cluster.id)
            )
                continue
            top = min(top, node.y - node.h / 2 - node.rankPadding)
            bottom = max(bottom, node.y + node.h / 2 + node.rankPadding)
        }

        for (child in graph.clusters) {
            val box = if (child.parent == cluster.id) boxes[child.id] else null
            if (box != null) {
                top = min(top, box.y)
                bottom = max(bottom, box.y + box.h)
            }
        }

        val title = titles.getValue(cluster.id).height + 4
        top -= CLUSTER_PAD + (if (graph.direction == Direction.TB) title else 0.0)
        bottom += CLUSTER_PAD + (if (graph.direction == Direction.BT) title else 0.0)
        boxes[cluster.id] =
            Box(entry.left[0].x, top, entry.right[0].x - entry.left[0].x, bottom - top)
    }

    // Layout extents and the transform into the drawing's own direction.
    var minX = Double.POSITIVE_INFINITY
    var maxX = Double.NEGATIVE_INFINITY
    var maxY = 0.0
    for (node in nodes.values) {
        minX = min(minX, node.x - node.w / 2)
        maxX = max(maxX, node.x + node.w / 2 + node.extra)
        maxY = max(maxY, node.y + node.h / 2 + node.rankPadding)
    }

    for (box in boxes.values) {
        minX = min(minX, box.x)
        maxX = max(maxX, box.x + box.w)
        maxY = max(maxY, box.y + box.h)
    }

    val titleBlock = graph.title?.let { textBlock(it, TITLE_FONT, 600, 480.0, measure) }
    val titleSpace = if (titleBlock != null) titleBlock.height + 14 else 0.0
    val extentX = maxX - minX
    val extentY = maxY
    val graphWidth = (if (horizontal) extentY else extentX) + MARGIN * 2
    val titleWidth = if (titleBlock != null) titleBlock.width + MARGIN * 2 else 0.0
    val horizontalMargin = MARGIN + max(0.0, (titleWidth - graphWidth) / 2)
    fun transform(point: Pt): Pt {
        val lx = point.x - minX
        return when (graph.direction) {
            Direction.BT -> Pt(lx + horizontalMargin, extentY - point.y + MARGIN + titleSpace)
            Direction.LR -> Pt(point.y + horizontalMargin, lx + MARGIN + titleSpace)
            Direction.RL -> Pt(extentY - point.y + horizontalMargin, lx + MARGIN + titleSpace)
            Direction.TB -> Pt(lx + horizontalMargin, point.y + MARGIN + titleSpace)
        }
    }

    val width = ceil(max(graphWidth, titleWidth))
    val height = ceil((if (horizontal) extentX else extentY) + MARGIN * 2 + titleSpace)
    fun finalBox(box: Box): Box {
        val a = transform(Pt(box.x, box.y))
        val b = transform(Pt(box.x + box.w, box.y + box.h))
        return Box(min(a.x, b.x), min(a.y, b.y), abs(b.x - a.x), abs(b.y - a.y))
    }

    val items = mutableListOf<SceneItem>()
    val backgrounds = mutableListOf<SceneItem>()
    val edgeItems = mutableListOf<SceneItem>()
    val labelItems = mutableListOf<SceneItem>()
    val titleItems = mutableListOf<SceneItem>()
    val nodeItems = mutableListOf<SceneItem>()

    for ((target, notes) in annotations) {
        val node = nodes.getValue(target)
        val center = transform(Pt(node.x, node.y))
        for (note in notes) {
            val at = nodes.getValue(note.id)
            val position = transform(Pt(at.x, at.y))
            val sign = if (note.annotation!!.side == NotePlacement.LEFT) -1 else 1
            edgeItems.add(
                PathItem(
                    d =
                        listOf(
                            PathCommand.Move(center.x + sign * node.box!!.w / 2, center.y),
                            PathCommand.Line(position.x - sign * at.box!!.w / 2, position.y),
                        ),
                    stroke = DiagramPaint.Tone("edge"),
                    width = 1.0,
                    dash = listOf(4.0, 4.0),
                )
            )
        }
    }

    if (titleBlock != null) {
        items.add(
            TextItem(
                x = width / 2,
                y = MARGIN + titleBlock.height / 2,
                lines = titleBlock.lines,
                size = TITLE_FONT,
                weight = 600,
                anchor = TextAnchor.MIDDLE,
                fill = tone("ink"),
            )
        )
    }

    for (cluster in graph.clusters.sortedWith { a, b -> clusterDepth(a.id) - clusterDepth(b.id) }) {
        val box = boxes[cluster.id] ?: continue
        val area = finalBox(box)
        val style = cluster.style
        val fill = style?.fill?.let { DiagramPaint.Literal(it) } ?: tone("cluster")
        backgrounds.add(
            PathItem(
                d = roundedRect(area.x, area.y, area.w, area.h, 14.0),
                fill = fill,
                stroke = style?.stroke?.let { DiagramPaint.Literal(it) } ?: tone("cluster-line"),
                width = style?.strokeWidth ?: 1.0,
                dash = if (style?.dashed == true) listOf(5.0, 4.0) else null,
            )
        )
        // Titles sit above edges, on a patch of the box's own fill, so lines never cross them.
        val title = titles.getValue(cluster.id)
        if (title.lines.isNotEmpty()) {
            val x = area.x + CLUSTER_PAD - 2
            val y = area.y + 10 + title.height / 2
            titleItems.add(
                PathItem(
                    d =
                        roundedRect(
                            x - 4,
                            y - title.height / 2 - 1,
                            title.width + 8,
                            title.height + 2,
                            5.0,
                        ),
                    fill = fill,
                )
            )
            titleItems.add(
                TextItem(
                    x = x,
                    y = y,
                    lines = title.lines,
                    size = CLUSTER_FONT,
                    weight = 600,
                    anchor = TextAnchor.START,
                    fill = textOn(style, "cluster-ink"),
                )
            )
        }
    }

    // Edges are cubic segments with tangents along the rank axis, joined at chain points.
    fun axis(sign: Double): Pt =
        when (graph.direction) {
            Direction.BT -> Pt(0.0, -sign)
            Direction.LR -> Pt(sign, 0.0)
            Direction.RL -> Pt(-sign, 0.0)
            Direction.TB -> Pt(0.0, sign)
        }

    fun edgePaint(edge: GraphEdge): DiagramPaint =
        edge.style?.stroke?.let { DiagramPaint.Literal(it) } ?: tone("edge")
    fun edgeWidth(edge: GraphEdge) =
        edge.style?.strokeWidth ?: (if (edge.line == EdgeLine.THICK) 3.0 else 1.5)

    val nodeOutlines = HashMap<String, List<List<PathCommand>>>()
    fun outlineFor(node: LNode): List<List<PathCommand>> =
        nodeOutlines.getOrPut(node.key) {
            val box = node.box!!
            shapePaths(node.node!!.shape, 0.0, 0.0, box.w, box.h).outlines
        }

    for (edge in edges) {
        val source = edge.edge
        if (source.line == EdgeLine.INVISIBLE) continue
        val chain = edge.chain
        val first = chain[0]
        val last = chain.last()
        fun port(node: LNode, toward: Double, side: Int): Pt {
            val shape = node.node?.shape ?: return Pt(node.x, node.y)
            val w = node.box?.let { if (horizontal) it.h else it.w } ?: node.w
            val round =
                shape == NodeShape.CIRCLE ||
                    shape == NodeShape.DOUBLE_CIRCLE ||
                    shape == NodeShape.DIAMOND ||
                    shape == NodeShape.CHOICE ||
                    shape == NodeShape.START ||
                    shape == NodeShape.END
            val reach = max(0.0, if (round) w * 0.2 else w / 2 - 12)
            val dx = max(-reach, min(reach, toward - node.x))
            val depth =
                outlineExtents(outlineFor(node), graph.direction, 'y', side, listOf(dx)).first()
            return Pt(node.x + dx, node.y + side * depth)
        }

        val startPoint = port(first, (chain.getOrNull(1) ?: last).x, 1)
        val endPoint = port(last, (chain.getOrNull(chain.size - 2) ?: first).x, -1)
        var points =
            listOf(startPoint) + chain.subList(1, chain.size - 1).map { Pt(it.x, it.y) } + endPoint

        // Links to a subgraph stop at its border.
        val fromCluster = if (edge.reversed) source.to else source.from
        val toCluster = if (edge.reversed) source.from else source.to
        if (clusters.containsKey(toCluster) && boxes.containsKey(toCluster)) {
            val box = boxes.getValue(toCluster)
            val outside = points.filter { it.y < box.y }
            val previous = outside.lastOrNull() ?: points[0]
            points = outside + Pt(max(box.x + 12, min(box.x + box.w - 12, previous.x)), box.y)
        }

        if (clusters.containsKey(fromCluster) && boxes.containsKey(fromCluster)) {
            val box = boxes.getValue(fromCluster)
            val outside = points.filter { it.y > box.y + box.h }
            val next = outside.firstOrNull() ?: points.last()
            points =
                listOf(Pt(max(box.x + 12, min(box.x + box.w - 12, next.x)), box.y + box.h)) +
                    outside
        }

        if (points.size < 2) continue
        val startHead = if (edge.reversed) source.end else source.start
        val endHead = if (edge.reversed) source.start else source.end
        // Pull the curve back so heads sit at the outline instead of under the stroke.
        val startInset = headInset(startHead)
        val endInset = headInset(endHead)
        val tipStart = points[0]
        val tipEnd = points.last()
        points =
            points.toMutableList().also {
                it[0] = Pt(tipStart.x, tipStart.y + startInset)
                it[it.size - 1] = Pt(tipEnd.x, tipEnd.y - endInset)
            }

        val d = smoothPath(points, ::transform)
        val paint = edgePaint(source)
        val strokeWidth = edgeWidth(source)
        val dashed = source.line == EdgeLine.DOTTED || source.style?.dashed == true
        edgeItems.add(
            PathItem(
                d = d,
                stroke = paint,
                width = strokeWidth,
                dash = if (dashed) listOf(4.0, 4.0) else null,
            )
        )
        edgeItems.addAll(headPaths(endHead, transform(tipEnd), axis(1.0), paint, strokeWidth))
        edgeItems.addAll(headPaths(startHead, transform(tipStart), axis(-1.0), paint, strokeWidth))

        val labelNode = chain.find { it.kind == LKind.LABEL }
        if (edge.label != null) {
            val middle = points[points.size / 2]
            val before = points.getOrNull(points.size / 2 - 1) ?: middle
            val at =
                if (labelNode != null) Pt(labelNode.x, labelNode.y)
                else Pt((middle.x + before.x) / 2, (middle.y + before.y) / 2)
            labelItems.addAll(edgeLabel(edge.label, transform(at)))
        }
    }

    // Links between nodes of one rank run straight across it.
    for (edge in flats) {
        val source = edge.edge
        if (source.line == EdgeLine.INVISIBLE) continue
        val pair =
            listOf(nodes.getValue(edge.u), nodes.getValue(edge.v)).sortedWith { left, right ->
                sortOrder(left.x - right.x)
            }
        val a = pair[0]
        val b = pair[1]
        val forward = a.key == (if (edge.reversed) edge.v else edge.u)
        val startHead = if (forward) source.start else source.end
        val endHead = if (forward) source.end else source.start
        val tipStart = Pt(a.x + a.w / 2 + a.extra, a.y)
        val tipEnd = Pt(b.x - b.w / 2, b.y)
        val from = Pt(tipStart.x + headInset(startHead), a.y)
        val to = Pt(tipEnd.x - headInset(endHead), b.y)
        val third = (to.x - from.x) / 3
        val paint = edgePaint(source)
        val strokeWidth = edgeWidth(source)
        val origin = transform(Pt(0.0, 0.0))
        val unit = transform(Pt(1.0, 0.0))
        val across = Pt(unit.x - origin.x, unit.y - origin.y)
        val start = transform(from)
        val control1 = transform(Pt(from.x + third, from.y))
        val control2 = transform(Pt(to.x - third, to.y))
        val end = transform(to)
        edgeItems.add(
            PathItem(
                d =
                    listOf(
                        PathCommand.Move(start.x, start.y),
                        PathCommand.Cubic(
                            control1.x,
                            control1.y,
                            control2.x,
                            control2.y,
                            end.x,
                            end.y,
                        ),
                    ),
                stroke = paint,
                width = strokeWidth,
                dash =
                    if (source.line == EdgeLine.DOTTED || source.style?.dashed == true)
                        listOf(4.0, 4.0)
                    else null,
            )
        )
        edgeItems.addAll(headPaths(endHead, transform(tipEnd), across, paint, strokeWidth))
        edgeItems.addAll(
            headPaths(startHead, transform(tipStart), Pt(-across.x, -across.y), paint, strokeWidth)
        )
        if (edge.label != null)
            labelItems.addAll(
                edgeLabel(
                    edge.label,
                    transform(Pt((from.x + to.x) / 2, a.y - edge.label.height / 2 - 6)),
                )
            )
    }

    for (loop in loops) {
        val node = loop.node
        val edge = loop.edge
        val right = node.x + node.w / 2
        val reach = loop.reach
        val bend = (24 + min(node.h, 40.0) / 2) * 0.7
        val offset = min(node.h / 4, 10.0)
        val top = node.y - offset
        val bottom = node.y + offset
        val sides =
            outlineExtents(outlineFor(node), graph.direction, 'x', 1, listOf(-offset, offset))
        val startX = node.x + sides[0]
        val endX = node.x + sides[1]
        val inset = headInset(edge.end)
        val start = transform(Pt(startX + headInset(edge.start), top))
        val control1 = transform(Pt(right + reach, top - bend))
        val control2 = transform(Pt(right + reach, bottom + bend))
        val end = transform(Pt(endX + inset, bottom))
        val d =
            listOf(
                PathCommand.Move(start.x, start.y),
                PathCommand.Cubic(control1.x, control1.y, control2.x, control2.y, end.x, end.y),
            )
        val paint = edgePaint(edge)
        val strokeWidth = edgeWidth(edge)
        if (edge.line != EdgeLine.INVISIBLE) {
            edgeItems.add(
                PathItem(
                    d = d,
                    stroke = paint,
                    width = strokeWidth,
                    dash =
                        if (edge.line == EdgeLine.DOTTED || edge.style?.dashed == true)
                            listOf(4.0, 4.0)
                        else null,
                )
            )
            val origin = transform(Pt(0.0, 0.0))
            val back = transform(Pt(-1.0, 0.0))
            edgeItems.addAll(
                headPaths(
                    edge.end,
                    transform(Pt(endX, bottom)),
                    Pt(back.x - origin.x, back.y - origin.y),
                    paint,
                    strokeWidth,
                )
            )
            edgeItems.addAll(
                headPaths(
                    edge.start,
                    transform(Pt(startX, top)),
                    Pt(back.x - origin.x, back.y - origin.y),
                    paint,
                    strokeWidth,
                )
            )
            if (loop.label != null) {
                val at =
                    transform(
                        Pt(
                            right +
                                reach * 0.75 +
                                (if (horizontal) loop.label.height else loop.label.width) / 2 +
                                6,
                            node.y,
                        )
                    )
                labelItems.addAll(edgeLabel(loop.label, at))
            }
        }
    }

    for (node in nodes.values) {
        val source = node.node ?: continue
        val box = node.box ?: continue
        val center = transform(Pt(node.x, node.y))
        val kind = family(source.shape)
        val style = source.style
        val solid = source.shape == NodeShape.START || source.shape == NodeShape.FORK
        val paths = shapePaths(source.shape, center.x, center.y, box.w, box.h)
        val fill: DiagramPaint =
            style?.fill?.let { DiagramPaint.Literal(it) } ?: if (solid) tone("ink") else tone(kind)
        val stroke: DiagramPaint =
            style?.stroke?.let { DiagramPaint.Literal(it) }
                ?: if (solid) tone("ink") else tone("$kind-line")
        for (d in paths.outlines) {
            nodeItems.add(
                PathItem(
                    d = d,
                    fill = fill,
                    stroke = stroke,
                    width =
                        style?.strokeWidth ?: (if (source.shape == NodeShape.END) 1.5 else 1.25),
                    dash = if (style?.dashed == true) listOf(5.0, 4.0) else null,
                    link = source.link,
                    shadow = !(solid || source.shape == NodeShape.END),
                )
            )
        }

        for (d in paths.details) {
            val filled = source.shape == NodeShape.END
            nodeItems.add(
                PathItem(
                    d = d,
                    fill = if (filled) tone("ink") else null,
                    stroke = if (filled) tone("ink") else stroke,
                    width = 1.25,
                    link = source.link,
                )
            )
        }

        if (
            box.text.lines.isNotEmpty() &&
                source.shape != NodeShape.START &&
                source.shape != NodeShape.END &&
                source.shape != NodeShape.FORK &&
                source.shape != NodeShape.CHOICE
        ) {
            nodeItems.add(
                TextItem(
                    x = center.x,
                    y = center.y + (if (source.shape == NodeShape.CYLINDER) 5 else 0),
                    lines = box.text.lines,
                    size = NODE_FONT,
                    weight = 500,
                    anchor = TextAnchor.MIDDLE,
                    fill = textOn(style, "$kind-ink"),
                    link = source.link,
                )
            )
        }
    }

    items.addAll(backgrounds)
    items.addAll(edgeItems)
    items.addAll(titleItems)
    items.addAll(labelItems)
    items.addAll(nodeItems)
    val count = graph.nodes.size
    return fitScene(
        Scene(
            width,
            height,
            items,
            graph.title ?: "Flowchart with $count ${if (count == 1) "node" else "nodes"}",
        ),
        measure,
    )
}

/**
 * A smooth curve through layout points. Ends leave along the rank axis; interior points take
 * Catmull-Rom tangents, so collinear runs stay straight.
 */
private fun smoothPath(points: List<Pt>, transform: (Pt) -> Pt): List<PathCommand> {
    val tangents = points.mapIndexed { index, point ->
        when (index) {
            0 -> Pt(0.0, (points[1].y - point.y) * 1.5)
            points.size - 1 -> Pt(0.0, (point.y - points[index - 1].y) * 1.5)
            else -> {
                val previous = points[index - 1]
                val next = points[index + 1]
                Pt((next.x - previous.x) / 2, (next.y - previous.y) / 2)
            }
        }
    }
    val start = transform(points[0])
    val d = mutableListOf<PathCommand>(PathCommand.Move(start.x, start.y))
    for (index in 1 until points.size) {
        val a = points[index - 1]
        val b = points[index]
        val m = tangents[index - 1]
        val n = tangents[index]
        val control1 = transform(Pt(a.x + m.x / 3, a.y + m.y / 3))
        val control2 = transform(Pt(b.x - n.x / 3, b.y - n.y / 3))
        val end = transform(b)
        d.add(PathCommand.Cubic(control1.x, control1.y, control2.x, control2.y, end.x, end.y))
    }

    return d
}

private fun edgeLabel(label: TextBlock, at: Pt): List<SceneItem> {
    val w = label.width + 12
    val h = label.height + 4
    return listOf(
        PathItem(d = roundedRect(at.x - w / 2, at.y - h / 2, w, h, 6.0), fill = tone("label")),
        TextItem(
            x = at.x,
            y = at.y,
            lines = label.lines,
            size = LABEL_FONT,
            weight = 500,
            anchor = TextAnchor.MIDDLE,
            fill = tone("muted"),
        ),
    )
}
