import type { IconName } from './icons'

export interface SelectOption {
  value: string
  label: string
  description?: string
  group?: string
  keywords?: string[]
  icon?: IconName
  disabled?: boolean
}
export interface SelectRow {
  key: string
  top: number
  height: number
  group?: string
  option?: SelectOption
  optionIndex?: number
}
export function searchText(value: string) {
  return value.normalize('NFD').replace(/\p{M}/gu, '').toLocaleLowerCase()
}

export function filterOptions(options: SelectOption[], query: string) {
  const words = searchText(query).trim().split(/\s+/).filter(Boolean)
  if (!words.length)
    return options
  return options.filter((option) => {
    const text = searchText([option.label, option.description, option.group, ...(option.keywords ?? [])].join(' '))
    return words.every(word => text.includes(word))
  })
}

export function selectRows(options: SelectOption[]) {
  const groups = new Map<string, SelectOption[]>()
  for (const option of options) {
    const key = option.group ?? ''
    if (!groups.has(key))
      groups.set(key, [])
    groups.get(key)!.push(option)
  }

  const rows: SelectRow[] = []
  const ordered: SelectOption[] = []
  let top = 0
  for (const [group, items] of groups) {
    if (group) {
      rows.push({
        key: `group:${group}`,
        top,
        height: 30,
        group,
      })
      top += 30
    }

    for (const option of items) {
      const height = option.description ? 62 : 44
      rows.push({
        key: `option:${option.value}`,
        top,
        height,
        option,
        optionIndex: ordered.length,
      })
      ordered.push(option)
      top += height
    }
  }

  return { rows, options: ordered, height: top }
}

export function visibleRows(rows: SelectRow[], scrollTop: number, height: number) {
  let low = 0
  let high = rows.length
  while (low < high) {
    const mid = (low + high) >>> 1
    if (rows[mid].top + rows[mid].height < scrollTop)
      low = mid + 1
    else
      high = mid
  }

  const start = Math.max(0, low - 3)
  let end = low
  while (end < rows.length && rows[end].top < scrollTop + height)
    end++
  return rows.slice(start, end + 3)
}
