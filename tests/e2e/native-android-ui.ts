interface NativeNode {
  attributes: Record<string, string>
  enabled: boolean
  parent?: NativeNode
}

function decodeAttribute(value: string): string {
  const entities: Record<string, string> = {
    amp: '&',
    lt: '<',
    gt: '>',
    quot: '"',
    apos: '\'',
  }
  return value.replace(/&(#x[\da-f]+|#\d+|amp|lt|gt|quot|apos);/gi, (match, name: string) => {
    if (!name.startsWith('#'))
      return entities[name] ?? match
    const code = name.startsWith('#x') ? Number.parseInt(name.slice(2), 16) : Number.parseInt(name.slice(1), 10)
    return code <= 0x10FFFF ? String.fromCodePoint(code) : match
  })
}

/** UIAutomator nodes, including the effective state inherited from containers. */
export function nativeNodes(document: string): NativeNode[] {
  const nodes: NativeNode[] = []
  const stack: NativeNode[] = []
  const tags = document.match(/<\/?node\b(?:[^"'<>]|"[^"]*"|'[^']*')*>/g) ?? []
  for (const tag of tags) {
    if (tag.startsWith('</')) {
      stack.pop()
      continue
    }

    const attributes = Object.fromEntries([...tag.matchAll(/([\w:-]+)=(?:"([^"]*)"|'([^']*)')/g)].map(match => [match[1], decodeAttribute(match[2] ?? match[3])]))
    const parent = stack.at(-1)
    const node: NativeNode = { attributes, parent, enabled: attributes.enabled !== 'false' && (parent?.enabled ?? true) }
    nodes.push(node)
    if (!tag.endsWith('/>'))
      stack.push(node)
  }

  return nodes
}

function editableAncestor(node: NativeNode): NativeNode | undefined {
  if (node.attributes.class === 'android.widget.EditText')
    return node
  return node.parent ? editableAncestor(node.parent) : undefined
}

function matchingNodes(document: string, text: string, preferEditable: boolean): NativeNode[] {
  const matches = nativeNodes(document).filter(node => [node.attributes.text, node.attributes['content-desc'], node.attributes.hint].includes(text))
  // A field description/label may be a child of EditText, while a separate
  // caption with identical text is not an editable target at all.
  return preferEditable ? matches.map(editableAncestor).filter((node): node is NativeNode => !!node) : matches
}

export function nativeInputFocused(document: string, label: string): boolean {
  return matchingNodes(document, label, true).some(node => node.enabled && node.attributes.focused === 'true')
}

function targetIn(document: string, text: string, includeClipped = false, preferEditable = false): { position: number[], signature: string } | null {
  for (const node of matchingNodes(document, text, preferEditable)) {
    if (!node.enabled)
      continue
    const bounds = node.attributes.bounds?.match(/^\[(-?\d+),(-?\d+)\]\[(-?\d+),(-?\d+)\]$/)?.slice(1).map(Number)
    if (!bounds || bounds[2] - bounds[0] < 20 || bounds[3] - bounds[1] < (includeClipped ? 1 : 30))
      continue
    const center = [Math.floor((bounds[0] + bounds[2]) / 2), Math.floor((bounds[1] + bounds[3]) / 2)]
    if (center[1] >= 64 && center[1] <= 1160) {
      const geometry: string[] = []
      let ancestor: NativeNode | undefined = node
      while (ancestor) {
        geometry.push(ancestor.attributes.bounds ?? '')
        ancestor = ancestor.parent
      }

      return { position: center, signature: geometry.join('|') }
    }
  }

  return null
}

export function nativePositionIn(document: string, text: string, includeClipped = false, preferEditable = false): number[] | null {
  return targetIn(document, text, includeClipped, preferEditable)?.position ?? null
}

export function createNativeTargetTracker() {
  let previous = ''
  return (document: string, text: string, preferEditable = false): number[] | null => {
    const target = targetIn(document, text, false, preferEditable)
    const signature = target?.signature ?? ''
    const stable = !!target && signature === previous
    previous = signature
    return stable ? target.position : null
  }
}
