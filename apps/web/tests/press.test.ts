import { describe, expect, it } from 'vitest'
import {
  releasedOnControl,
  sameBox,
  stopsRowFling,
  touchSlop,
  visibleBox,
} from '../src/press'

const pressed = {
  left: 10,
  top: 500,
  right: 210,
  bottom: 544,
}
const moved = {
  left: 10,
  top: 570,
  right: 210,
  bottom: 614,
}

describe('releasedOnControl', () => {
  it('accepts a release where the control was pressed after live content moved it', () => {
    expect(releasedOnControl(pressed, moved, 110.4, 522.6)).toBe(true)
    // Pointer jitter during the press stays within the pressed box.
    expect(releasedOnControl(pressed, moved, 111, 524)).toBe(true)
  })

  it('accepts a release that follows the control to its new position', () => {
    expect(releasedOnControl(pressed, moved, 110, 600)).toBe(true)
  })

  it('accepts a touch that drifts a little past the edge of an unmoved control', () => {
    expect(releasedOnControl(pressed, pressed, 110, 498, touchSlop)).toBe(true)
    expect(releasedOnControl(pressed, pressed, 110, 498)).toBe(false)
    expect(releasedOnControl(pressed, pressed, 110, 500 - touchSlop - 1, touchSlop)).toBe(false)
  })

  it('ignores a release dragged away from both positions', () => {
    expect(releasedOnControl(pressed, moved, 110, 400)).toBe(false)
    expect(releasedOnControl(pressed, pressed, 260, 522)).toBe(false)
  })
})

describe('visibleBox', () => {
  it('clips a control scrolled partly out of its container', () => {
    const chip = {
      left: 380,
      top: 10,
      right: 480,
      bottom: 42,
    }
    const row = {
      left: 100,
      top: 6,
      right: 420,
      bottom: 46,
    }
    expect(visibleBox(chip, row)).toEqual({
      left: 380,
      top: 10,
      right: 420,
      bottom: 42,
    })
    expect(releasedOnControl(visibleBox(chip, row), visibleBox(chip, row), 450, 26)).toBe(false)
  })

  it('keeps an unclipped control whole', () => {
    expect(visibleBox(pressed, undefined)).toEqual(pressed)
  })
})

describe('stopsRowFling', () => {
  it('treats a touch or pen tap while the row still scrolls as stopping its fling', () => {
    expect(stopsRowFling('touch', 1040, 1000)).toBe(true)
    expect(stopsRowFling('pen', 1099, 1000)).toBe(true)
  })

  it('counts a touch once the row has settled', () => {
    expect(stopsRowFling('touch', 1100, 1000)).toBe(false)
    expect(stopsRowFling('touch', 10, Number.NEGATIVE_INFINITY)).toBe(false)
  })

  it('always counts a mouse press, even right after a wheel scroll', () => {
    expect(stopsRowFling('mouse', 1010, 1000)).toBe(false)
  })
})

describe('sameBox', () => {
  it('treats sub-pixel rounding as unmoved and a queued row as moved', () => {
    expect(sameBox(pressed, { ...pressed, top: 500.4, bottom: 544.4 })).toBe(true)
    expect(sameBox(pressed, moved)).toBe(false)
  })
})
