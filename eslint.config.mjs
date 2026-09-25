import antfu from '@antfu/eslint-config'

export default antfu({
  vue: true,
  typescript: true,
  ignores: ['docs/screenshots/**', '.data/**', 'playwright-report/**', 'test-results/**', 'android/**/build/**', 'android/.gradle/**', 'android/.kotlin/**'],
  rules: {
    'style/padding-line-between-statements': [
      'error',
      { blankLine: 'always', prev: '*', next: ['function', 'class', 'interface', 'type'] },
      { blankLine: 'always', prev: ['function', 'class', 'interface', 'type'], next: '*' },
      { blankLine: 'always', prev: 'multiline-block-like', next: '*' },
    ],
    'style/object-curly-newline': ['error', { multiline: true, minProperties: 4, consistent: true }],
    'style/object-property-newline': ['error', { allowAllPropertiesOnSameLine: true }],
    'style/max-statements-per-line': ['error', { max: 1 }],
    'style/nonblock-statement-body-position': ['error', 'below'],
    'vue/max-attributes-per-line': ['error', { singleline: 3, multiline: 1 }],
  },
})
