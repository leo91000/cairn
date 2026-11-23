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
// pointer stays over the control where it was pressed, or ends over where it is now, within
// [slop]: a touch drifts a little across the edge of what it taps.
export function releasedOnControl(pressed: Box, current: Box, x: number, y: number, slop = 0): boolean {
  const inside = (box: Box) => x >= box.left - slop && x <= box.right + slop
    && y >= box.top - slop && y <= box.bottom + slop
  return inside(pressed) || inside(current)
}

// How far a touch or pen release may drift past the control and still count.
export const touchSlop = 16

// A touch or pen tap that stops a scrolling row's fling is not a press: browsers send no click
// for it. A mouse press never stops a fling, so it always counts.
export function stopsRowFling(pointerType: string, pressedAt: number, rowScrolledAt: number): boolean {
  return pointerType !== 'mouse' && pressedAt - rowScrolledAt < 100
}

export interface Origin {
  x: number
  y: number
}

// Whether live content moved a control from where it was pressed, give or take a pixel of
// rounding. A control that resizes or that its container clips differently has not moved.
export function sameOrigin(a: Origin, b: Origin): boolean {
  return Math.abs(a.x - b.x) < 1 && Math.abs(a.y - b.y) < 1
}
