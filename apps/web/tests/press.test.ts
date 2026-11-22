import { describe, expect, it } from 'vitest'
import { releasedOnControl, visibleBox } from '../src/press'

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
