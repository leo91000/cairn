import { defineConfig } from '@playwright/test'
import ordinary from './playwright.config'

// Physical cache qualification is separate from the ordinary projects. Their
// default Chromium launch explicitly disables BFCache. Change only that switch.
export default defineConfig({
  ...ordinary,
  projects: ordinary.projects!.filter(project => ['journeys-mcps', 'layout-webkit-mcps'].includes(project.name!)).map(project => ({
    ...project,
    testMatch: 'availability-bfcache.spec.ts',
    use: {
      ...project.use,
      ...(project.name === 'journeys-mcps' ? { launchOptions: { ignoreDefaultArgs: ['--disable-back-forward-cache'] } } : {}),
    },
  })),
})
