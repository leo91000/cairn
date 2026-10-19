import { fileURLToPath } from 'node:url'
import { defineConfig } from 'vite'

export default defineConfig({
  define: { 'process.env.NODE_ENV': '"production"' },
  build: {
    outDir: fileURLToPath(new URL('../test-results/qualification-bundle', import.meta.url)),
    emptyOutDir: true,
    lib: {
      entry: fileURLToPath(new URL('e2e/network-qualification-harness.ts', import.meta.url)),
      name: 'CairnQualification',
      formats: ['iife'],
      fileName: () => 'harness.js',
    },
  },
})
