import type { Page } from '@playwright/test'

// Route handlers can outlive the last UI assertion. Settle them while the page
// and its request context still exist, including when a UI assertion fails.
export async function withSettledRoutes(page: Page, body: () => Promise<void>) {
  try {
    await body()
  }
  finally {
    await page.unrouteAll({ behavior: 'wait' })
  }
}
