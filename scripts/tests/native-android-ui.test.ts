import { expect, it } from 'vitest'
import { nativeInputFocused, nativeNodes, nativePositionIn } from '../../tests/e2e/native-android-ui'

it('targets the described input rather than its separate caption', () => {
  const document = `<hierarchy><node class="android.view.View" enabled="true">
    <node text="Valeur à mémoriser" class="android.widget.TextView" bounds="[40,753][283,789]"/>
    <node text='"done"' class="android.widget.EditText" enabled="true" bounds="[40,909][680,1021]">
      <node content-desc="Valeur à mémoriser" bounds="[40,909][680,1021]"/>
    </node>
  </node></hierarchy>`
  expect(nativePositionIn(document, 'Valeur à mémoriser', false, true)).toEqual([360, 965])
  expect(nativePositionIn(document, 'Valeur à mémoriser')).toEqual([161, 771])
  expect(nativePositionIn('<node text="Caption" class="android.widget.TextView" bounds="[40,753][283,789]"/>', 'Caption', false, true)).toBeNull()
})

it('inherits disabled state even when the button text node reports enabled', () => {
  const document = '<node class="android.view.View" enabled="false"><node text="Enregistrer" enabled="true" bounds="[510,164][648,200]"/></node>'
  expect(nativeNodes(document).find(node => node.attributes.text === 'Enregistrer')?.enabled).toBe(false)
  expect(nativePositionIn(document, 'Enregistrer')).toBeNull()
})

it('reads entered JSON from single or double quoted XML attributes', () => {
  for (const value of [`text='{"count":'`, 'text="{&quot;count&quot;:"']) {
    const nodes = nativeNodes(`<node class="android.widget.EditText" focused="true" ${value}/>`)
    expect(nodes[0].attributes.text).toBe('{"count":')
  }

  expect(nativeNodes('<node text="a&gt;b &amp; &#x1f600;"/>')[0].attributes.text).toBe('a>b & 😀')
})

it('waits for the requested input to take focus rather than another field', () => {
  const document = `<node class="android.widget.EditText" focused="true" text="previous"><node content-desc="Previous"/></node>
    <node class="android.widget.EditText" focused="false" text="next"><node content-desc="Next"/></node>`
  expect(nativeInputFocused(document, 'Previous')).toBe(true)
  expect(nativeInputFocused(document, 'Next')).toBe(false)
  expect(nativeInputFocused(document.replace('focused="false"', 'focused="true"'), 'Next')).toBe(true)
})
