import type { SettingField } from './plugins'

/** Only typing may populate a secret draft, including keys such as constructor. */
export function pluginFormSecrets(): Record<string, string> {
  return Object.create(null)
}

/** A package's answer for one field; keys such as constructor never read Object.prototype. */
export function pluginFieldAnswer<T>(answers: Record<string, T> | undefined, key: string): T | undefined {
  return answers && Object.hasOwn(answers, key) ? answers[key] : undefined
}

/** Native control values, with defaults applied only when opening a form. */
export function pluginFormValue(field: SettingField, saved: unknown): unknown {
  const value = saved === undefined ? field.default : saved
  if (field.type === 'boolean')
    return String(value) === 'true'
  if (field.type === 'multiselect')
    return Array.isArray(value) ? [...value] : []
  return value !== null && value !== undefined && typeof value !== 'object' ? String(value) : ''
}

export function pluginFormValues(fields: SettingField[], saved: Record<string, unknown> = {}): Record<string, any> {
  return Object.fromEntries(fields.filter(field => field.type !== 'secret')
    .map((field) => {
      const savedValue = Object.hasOwn(saved, field.key) ? saved[field.key] : undefined
      return [field.key, pluginFormValue(field, savedValue)]
    }))
}

/** Declared settings only, including hidden derived fields; clearing never reapplies a default. */
export function pluginSettings(fields: SettingField[], values: Record<string, unknown>): Record<string, unknown> {
  const settings: Record<string, unknown> = {}
  for (const field of fields) {
    if (field.type === 'secret')
      continue
    const value = Object.hasOwn(values, field.key) ? values[field.key] : undefined
    if (field.type === 'boolean') {
      settings[field.key] = String(value) === 'true'
    }
    else if (field.type === 'multiselect') {
      settings[field.key] = Array.isArray(value) ? [...value] : []
    }
    else if (field.type === 'select') {
      // Choices are identifiers; the server and the package compare their exact bytes.
      if (typeof value === 'string' && value !== '')
        settings[field.key] = value
    }
    else {
      const text = value === null || value === undefined ? '' : String(value).trim()
      if (!text)
        continue
      if (field.type === 'number') {
        // Number() also accepts hexadecimal strings, unlike a native number control.
        const decimal = /^[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:e[+-]?\d+)?$/i.test(text)
        const number = Number(text)
        if (decimal && Number.isFinite(number))
          settings[field.key] = number
      }
      else {
        settings[field.key] = text
      }
    }
  }

  return settings
}

/** Blank secrets keep saved credentials. View actions receive the typed, untrimmed value. */
export function pluginSecrets(secrets: Record<string, string>, trim = true): Record<string, string> {
  return Object.fromEntries(Object.entries(secrets).filter(([, value]) => value.trim())
    .map(([key, value]) => [key, trim ? value.trim() : value]))
}
