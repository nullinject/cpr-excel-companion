import { readFile } from 'node:fs/promises'
import { fileURLToPath, URL } from 'node:url'
import CodexProxyUI from '@codex-proxy/ui/vite'
import tailwindcss from '@tailwindcss/vite'
import vue from '@vitejs/plugin-vue'
import { defineConfig } from 'vite'

export default defineConfig(({ mode, command }) => {
  const sourceUi = mode === 'source'
  const uiRoot = new URL('../../../../ui/', import.meta.url)
  return {
    resolve: {
      alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) },
    },
    define: {
      'process.env.NODE_ENV': JSON.stringify(command === 'serve' ? 'development' : 'production'),
    },
    plugins: [
      vue(),
      tailwindcss(),
      CodexProxyUI({ source: sourceUi ? uiRoot : undefined }),
      {
        name: 'plugin-page-entry',
        async generateBundle() {
          const template = await readFile(new URL('./index.html', import.meta.url), 'utf8')
          this.emitFile({
            type: 'asset',
            fileName: 'index.html',
            source: template
              .replace('</head>', '    <link rel="stylesheet" href="./app.css" />\n  </head>')
              .replace('<script type="module" src="/src/main.ts"></script>', '<script src="./app.js" defer></script>'),
          })
        },
      },
    ],
    build: {
      outDir: sourceUi ? '.vite/source-dist' : 'dist',
      lib: {
        entry: fileURLToPath(new URL('./src/main.ts', import.meta.url)),
        name: 'Workbench',
        formats: ['iife'],
        fileName: () => 'app.js',
        cssFileName: 'app',
      },
      cssCodeSplit: false,
      sourcemap: false,
    },
  }
})
