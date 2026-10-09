import { createReadStream, readdirSync, readFileSync } from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import tailwindcss from '@tailwindcss/vite'
import vue from '@vitejs/plugin-vue'
import { defineConfig } from 'vite'

const pdfRoot = path.dirname(fileURLToPath(import.meta.resolve('pdfjs-dist/package.json')))
const pdfVersion = JSON.parse(readFileSync(path.join(pdfRoot, 'package.json'), 'utf8')).version
const pdfAssets = ['cmaps', 'standard_fonts', 'wasm'].flatMap(directory => readdirSync(path.join(pdfRoot, directory)).map(name => ({ source: path.join(pdfRoot, directory, name), target: `pdfjs/${pdfVersion}/${directory}/${name}` })))

export default defineConfig({
  plugins: [vue(), tailwindcss(), {
    name: 'local-pdf-assets',
    generateBundle() {
      for (const asset of pdfAssets)
        this.emitFile({ type: 'asset', fileName: asset.target, source: readFileSync(asset.source) })
    },
    configureServer(server) {
      server.middlewares.use((request, response, next) => {
        const asset = pdfAssets.find(asset => `/${asset.target}` === request.url)
        if (!asset)
          return next()
        response.setHeader('Content-Type', asset.target.endsWith('.wasm') ? 'application/wasm' : 'application/octet-stream')
        createReadStream(asset.source).pipe(response)
      })
    },
  }],
  server: {
    port: 5178,
    proxy: {
      '/api': { target: 'http://127.0.0.1:4311', ws: true },
      '/oauth': 'http://127.0.0.1:4311',
    },
  },
  build: {
    chunkSizeWarningLimit: 400,
    rolldownOptions: {
      input: { default: 'apps/web/index.html', official: 'apps/web/beacon.html' },
      output: {
        // Both documents enter the same official account shell. Keep its initial dependencies
        // together so each document loads one shared bundle; views stay lazy.
        codeSplitting: { groups: [{ name: 'workspace', tags: ['$initial'], minShareCount: 2 }] },
      },
    },
  },
})
