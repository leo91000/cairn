const nullableKey = '_cairnAllowsNull'
const jsonWidgetKey = '_cairnJsonWidget'

// Project root object compositions onto editable inputs. This is a presentation
// index, not a replacement for the plugin's complete JSON Schema validation.
export function ruleToolInputSchema(root: any): any {
  const budget = { remaining: 512 }

  function collect(schema: any, depth: number): { properties: Record<string, any>, required: string[] } {
    const result = { properties: Object.create(null) as Record<string, any>, required: [] as string[] }
    if (!schema || typeof schema !== 'object' || depth > 8 || budget.remaining-- <= 0)
      return result
    if (typeof schema.$ref === 'string' && schema.$ref.startsWith('#/')) {
      const target = schema.$ref.slice(2).split('/').reduce((value: any, part: string) => value?.[part.replace(/~1/g, '/').replace(/~0/g, '~')], root)
      const { $ref: _ref, ...rest } = schema
      return collect({ ...target, ...rest }, depth + 1)
    }

    Object.assign(result.properties, schema.properties ?? {})
    result.required = Array.isArray(schema.required) ? [...schema.required] : []
    const merge = (properties: Record<string, any>, composition = 'allOf') => {
      for (const [name, field] of Object.entries(properties)) {
        const previous = result.properties[name]
        // Several constraints/variants on one input need a truthful JSON editor;
        // choosing one variant's widget could silently change the value's type.
        result.properties[name] = Object.hasOwn(result.properties, name) && JSON.stringify(previous) !== JSON.stringify(field)
          ? { title: previous?.title ?? field?.title, [composition]: [previous, field], [jsonWidgetKey]: true }
          : field
      }
    }

    for (const branch of schema.allOf ?? []) {
      const fields = collect(branch, depth + 1)
      merge(fields.properties)
      result.required.push(...fields.required)
    }

    for (const kind of ['oneOf', 'anyOf']) {
      if (!Array.isArray(schema[kind]) || !schema[kind].length)
        continue
      const branches = schema[kind].map((branch: any) => collect(branch, depth + 1))
      for (const branch of branches)
        merge(branch.properties, 'anyOf')
      result.required.push(...branches[0].required.filter((name: string) => branches.every((branch: { required: string[] }) => branch.required.includes(name))))
    }

    result.required = [...new Set(result.required)]
    for (const name of result.required) {
      if (!Object.hasOwn(result.properties, name))
        result.properties[name] = { [jsonWidgetKey]: true }
    }

    return result
  }

  return { ...root, ...collect(root, 0) }
}

export function resolveRuleSchema(schema: any, root = schema, depth = 0): any {
  return resolveSchema(schema, root, depth, { remaining: 512 })
}

function resolveSchema(schema: any, root: any, depth: number, budget: { remaining: number }): any {
  if (!schema || typeof schema !== 'object' || depth > 8 || budget.remaining-- <= 0)
    return {}
  if (typeof schema.$ref === 'string' && schema.$ref.startsWith('#/')) {
    const target = schema.$ref.slice(2).split('/').reduce((value: any, part: string) => value?.[part.replace(/~1/g, '/').replace(/~0/g, '~')], root)
    const { $ref: _ref, ...rest } = schema
    return resolveSchema({ ...target, ...rest }, root, depth + 1, budget)
  }

  const choices = schema.oneOf ?? schema.anyOf
  const resolvedChoices = Array.isArray(choices) ? choices.map(choice => resolveSchema(choice, root, depth + 1, budget)) : []
  const jsonWidget = schema[jsonWidgetKey] === true
    || (Array.isArray(schema.type) && schema.type.filter((kind: string) => kind !== 'null').length > 1)
    || resolvedChoices.filter(choice => choice.type !== 'null').length > 1
  const nullMatches = resolvedChoices.filter(choice => choice[nullableKey] === true).length
  const declaredNull = Object.hasOwn(schema, nullableKey) ? schema[nullableKey] === true : !schema.type || schema.type === 'null' || (Array.isArray(schema.type) && schema.type.includes('null'))
  const nullable = declaredNull
    && (!Array.isArray(choices) || (schema.oneOf ? nullMatches === 1 : nullMatches > 0))
    && (!Array.isArray(schema.allOf) || schema.allOf.every((choice: any) => resolveSchema(choice, root, depth + 1, budget)[nullableKey] === true))
    && (!Array.isArray(schema.enum) || schema.enum.includes(null))
    && (!Object.hasOwn(schema, 'const') || schema.const === null)
  if (!schema.type && Array.isArray(choices)) {
    return resolveSchema({
      ...schema,
      ...(resolvedChoices.find(entry => entry.type !== 'null') ?? resolvedChoices[0]),
      [nullableKey]: nullable,
      [jsonWidgetKey]: jsonWidget,
      oneOf: undefined,
      anyOf: undefined,
    }, root, depth + 1, budget)
  }

  const result = { ...schema, type: Array.isArray(schema.type) ? schema.type.find((kind: string) => kind !== 'null') ?? 'null' : schema.type }
  // UI annotation retains null intent after reducing a nullable union to its widget type.
  delete result[nullableKey]
  if (nullable || schema[nullableKey] === false)
    result[nullableKey] = nullable
  if (jsonWidget)
    result[jsonWidgetKey] = true
  return result
}

export function ruleSchemaAllowsNull(input: any, root = input): boolean {
  return resolveRuleSchema(input, root)[nullableKey] === true
}

export function ruleSchemaUsesJson(input: any, root = input): boolean {
  return resolveRuleSchema(input, root)[jsonWidgetKey] === true
}

export function ruleLiteralConcreteDefault(input: any, root = input): unknown {
  const schema = resolveRuleSchema(input, root)
  if (schema.type === 'null')
    return undefined
  if (Object.hasOwn(schema, 'const'))
    return schema.const === null ? undefined : JSON.parse(JSON.stringify(schema.const))
  if (Array.isArray(schema.enum))
    return schema.enum.some((value: unknown) => value !== null) ? JSON.parse(JSON.stringify(schema.enum.find((value: unknown) => value !== null))) : undefined
  if (schema.default !== null && schema.default !== undefined)
    return ruleLiteralDefault(schema, root)
  const { default: _default, ...withoutDefault } = schema
  return ruleLiteralDefault(withoutDefault, root)
}

export function ruleLiteralDefault(input: any, root = input, depth = 0): unknown {
  const schema = resolveRuleSchema(input, root)
  if ('const' in schema)
    return JSON.parse(JSON.stringify(schema.const))
  if ('default' in schema)
    return JSON.parse(JSON.stringify(schema.default))
  if (Array.isArray(schema.enum) && schema.enum.length)
    return JSON.parse(JSON.stringify(schema.enum[0]))
  switch (schema.type) {
    case 'null': return null
    case 'boolean': return false
    case 'number': case 'integer': return 0
    case 'object': return depth < 8
      ? Object.fromEntries((schema.required ?? []).filter((key: string) => schema.properties?.[key]).map((key: string) => [key, ruleLiteralDefault(schema.properties[key], root, depth + 1)]))
      : {}
    case 'array': return []
    default: return ''
  }
}
