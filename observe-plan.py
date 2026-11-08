from pathlib import Path
p=Path('tests/e2e/claude.spec.ts')
s=p.read_text()
needle="test('plan drafts keep their mode across conversation switches and reloads', async ({ page, workspace }) => {"
s=s.replace(needle,needle+'''
  await page.addInitScript(() => {
    const events: unknown[] = []
    Object.assign(window, { planProbe: events })
    for (const type of ['mousedown', 'mouseup', 'click']) {
      document.addEventListener(type, (event) => {
        const target = event.target as Element
        const row = target.closest('[data-fil-key]')
        if (!row && !target.closest('[role="tab"]')) return
        events.push({ type, time: performance.now(), tag: target.tagName, row: row?.getAttribute('data-fil-key'), button: target.closest('button')?.getAttribute('aria-label'), url: location.pathname, order: [...document.querySelectorAll('[data-fil-key]')].slice(0, 3).map(e => e.getAttribute('data-fil-key')) })
      }, true)
    }
  })
''')
s += '''
test.afterEach(async ({ page }) => {
  const probe = await page.evaluate(() => ({ url: location.pathname, events: (window as unknown as { planProbe: unknown[] }).planProbe })).catch(() => null)
  await test.info().attach('plan-pointer-probe', { body: JSON.stringify(probe), contentType: 'application/json' })
})
'''
p.write_text(s)
