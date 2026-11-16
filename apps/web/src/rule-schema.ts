const nullableKey = '_cairnAllowsNull'
const jsonWidgetKey = '_cairnJsonWidget'

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
