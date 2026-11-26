// Lays a parsed diagram out as a flat scene of paths and text. Renderers only map tones to
// theme colors, so the web and Android (DiagramLayout*.kt) draw the same geometry.

import type { Diagram } from './diagram'
import type { Scene, TextMeasure } from './diagram/scene'
import { DiagramError } from './diagram'
import { layoutGraph } from './diagram/graph-layout'
import { layoutPie } from './diagram/pie-layout'
import { MAX_SCENE_SIDE } from './diagram/scene'
import { layoutSequence } from './diagram/sequence-layout'

export { layoutGraph } from './diagram/graph-layout'
export { layoutPie } from './diagram/pie-layout'
export type {
  Paint,
  PathCommand,
  PathItem,
  Scene,
  SceneItem,
  TextItem,
  TextMeasure,
  Tone,
} from './diagram/scene'
export { lineHeight, wrapText } from './diagram/scene'
export { layoutSequence } from './diagram/sequence-layout'

export function layoutDiagram(diagram: Diagram, measure: TextMeasure): Scene {
  const scene = diagram.kind === 'graph'
    ? layoutGraph(diagram, measure)
    : diagram.kind === 'sequence'
      ? layoutSequence(diagram, measure)
      : layoutPie(diagram, measure)
  if (scene.width > MAX_SCENE_SIDE || scene.height > MAX_SCENE_SIDE)
    throw new DiagramError('This diagram is too large to draw.')
  return scene
}
