import { describe, expect, it } from 'vitest'
import { filterOptions, selectRows, visibleRows } from '../src/select'

describe('virtual select data', () => {
  it('searches words across names, descriptions, groups and keywords without accent sensitivity', () => {
    const options = [
      {
        value: 'a',
        label: 'Équipe CSS',
        description: 'Baseline maintenance',
        group: 'Development',
        keywords: ['styles'],
      },
      { value: 'b', label: 'Release engineer', description: 'Publish versions' },
    ]
    expect(filterOptions(options, 'equipe BASELINE')).toEqual([options[0]])
    expect(filterOptions(options, 'styles development')).toEqual([options[0]])
    expect(filterOptions(options, 'missing')).toEqual([])
    expect(filterOptions(options, '  ')).toBe(options)
  })
  it('bounds rendered rows for 10,000 rich options and preserves scroll geometry at both ends', () => {
    const options = Array.from({ length: 10000 }, (_, index) => ({
      value: `option-${index}`,
      label: `Agent ${index}`,
      description: 'Workspace maintainer',
      group: index < 5000 ? 'Team A' : 'Team B',
    }))
    const layout = selectRows(options)
    expect(layout.height).toBe(10000 * 62 + 60)
    const start = visibleRows(layout.rows, 0, 280)
    expect(start.length).toBeLessThan(15)
    expect(start[0].group).toBe('Team A')
    const end = visibleRows(layout.rows, layout.height - 280, 280)
    expect(end.length).toBeLessThan(15)
    expect(end.at(-1)?.option?.value).toBe('option-9999')
    const middle = visibleRows(layout.rows, 310000, 280)
    expect(middle.some(row => row.group === 'Team B')).toBe(true)
  })
})
