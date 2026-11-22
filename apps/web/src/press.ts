export interface Box {
  left: number
  top: number
  right: number
  bottom: number
}

// The part of a control its clipping container leaves visible.
export function visibleBox(control: Box, container: Box | undefined): Box {
  if (!container)
    return control

  return {
    left: Math.max(control.left, container.left),
    top: Math.max(control.top, container.top),
    right: Math.min(control.right, container.right),
    bottom: Math.min(control.bottom, container.bottom),
  }
}

// Live content can move a control between press and release. A release counts when the
// pointer stays over the control where it was pressed, or ends over where it is now.
export function releasedOnControl(pressed: Box, current: Box, x: number, y: number): boolean {
  const inside = (box: Box) => x >= box.left && x <= box.right && y >= box.top && y <= box.bottom
  return inside(pressed) || inside(current)
}
