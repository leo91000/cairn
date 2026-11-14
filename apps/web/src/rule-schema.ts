export function resolveRuleSchema(schema: any, root = schema, depth = 0): any {
  if (!schema || typeof schema !== 'object' || depth > 8)
    return {}
  if (typeof schema.$ref === 'string' && schema.$ref.startsWith('#/')) {
    const target = schema.$ref.slice(2).split('/').reduce((value: any, part: string) => value?.[part.replace(/~1/g, '/').replace(/~0/g, '~')], root)
    const { $ref: _ref, ...rest } = schema
    return resolveRuleSchema({ ...target, ...rest }, root, depth + 1)
  }

  const choices = schema.oneOf ?? schema.anyOf
  if (!schema.type && Array.isArray(choices)) {
    return resolveRuleSchema({
      ...schema,
      ...choices.find((entry: any) => entry.type !== 'null'),
      oneOf: undefined,
      anyOf: undefined,
    }, root, depth + 1)
  }

  return { ...schema, type: Array.isArray(schema.type) ? schema.type.find((kind: string) => kind !== 'null') : schema.type }
}

export function ruleLiteralDefault(input: any, root = input, depth = 0): unknown {
  const schema = resolveRuleSchema(input, root)
  if ('default' in schema)
    return JSON.parse(JSON.stringify(schema.default))
  if (Array.isArray(schema.enum) && schema.enum.length)
    return schema.enum[0]
  switch (schema.type) {
    case 'boolean': return false
    case 'number': case 'integer': return 0
    case 'object': return depth < 8
      ? Object.fromEntries((schema.required ?? []).filter((key: string) => schema.properties?.[key]).map((key: string) => [key, ruleLiteralDefault(schema.properties[key], root, depth + 1)]))
      : {}
    case 'array': return []
    default: return ''
  }
}
