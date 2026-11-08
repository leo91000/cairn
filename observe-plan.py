from pathlib import Path
p=Path('tests/e2e/claude.spec.ts')
s=p.read_text()
start=s.index("test('plan drafts keep their mode")
end=s.index("\ntest(",start+10)
case=s[start:end]
needle='  test.setTimeout(120000)'
case=case.replace(needle,needle+'''
  await useRelayForHttpMocks(page)
  await page.addInitScript(() => {
    const probe = { armed: false, held: [] as (() => void)[] }
    Object.assign(window, { planProbe: probe })
    const add = EventSource.prototype.addEventListener
    EventSource.prototype.addEventListener = function (type, listener, options) {
      if (type !== 'batch' || typeof listener !== 'function')
        return add.call(this, type, listener, options)
      return add.call(this, type, function (event) {
        const deliver = () => listener.call(this, event)
        if (probe.held.length || (probe.armed && (event as MessageEvent).data.includes('Inspect the saved Plan draft'))) {
          probe.held.push(deliver)
          return
        }
        deliver()
      }, options)
    }
    for (const type of ['mousedown', 'mouseup', 'click']) {
      document.addEventListener(type, (event) => {
        const target = event.target as Element
        const row = target.closest('[data-fil-key]')
        if (!row) return
        const events = JSON.parse(sessionStorage.getItem('plan-pointer-events') || '[]')
        events.push({ type, tag: target.tagName, row: row.getAttribute('data-fil-key'), button: target.closest('button')?.getAttribute('aria-label'), url: location.pathname })
        sessionStorage.setItem('plan-pointer-events', JSON.stringify(events))
      }, true)
    }
  })
''',1)
needle="  await page.getByRole('button', { name: 'Queue', exact: true }).click()"
case=case.replace(needle,"  await page.evaluate(() => (window as any).planProbe.armed = true)\n"+needle,1)
needle='  await fil.locator(`[data-fil-key="chat:${second.id}"]`).click()'
pos=case.rfind(needle)
forced='''  await expect.poll(() => page.evaluate(() => (window as any).planProbe.held.length)).toBeGreaterThan(0)
  const row = fil.locator(`[data-fil-key="chat:${second.id}"]`)
  const box = (await row.boundingBox())!
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2)
  await page.mouse.down()
  await page.evaluate(() => {
    const probe = (window as any).planProbe
    probe.armed = false
    for (const deliver of probe.held.splice(0)) deliver()
  })
  await expect(fil.locator('[data-fil-key]').first()).toHaveAttribute('data-fil-key', `chat:${first.id}`)
  await page.mouse.up()
'''
case=case[:pos]+case[pos:].replace(needle,forced,1)
s=s[:start]+case+s[end:]
s += '''
test.afterEach(async ({ page }) => {
  const probe = await page.evaluate(() => ({ url: location.pathname, events: JSON.parse(sessionStorage.getItem('plan-pointer-events') || '[]') })).catch(() => null)
  await test.info().attach('plan-pointer-probe', { body: JSON.stringify(probe), contentType: 'application/json' })
})
'''
p.write_text(s)
