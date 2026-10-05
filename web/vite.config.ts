import { svelte } from '@sveltejs/vite-plugin-svelte'
import tailwindcss from '@tailwindcss/vite'
import { defineConfig } from 'vitest/config'

// `npm run dev` serves the page with hot reload and passes /ws on to a running
// `chsmartbulb daemon --web 8378`; `npm run build` writes the bundle the daemon serves.
const daemon = process.env.CHSMARTBULB_WEB ?? 'ws://127.0.0.1:8378'

export default defineConfig({
  plugins: [svelte(), tailwindcss()],
  build: {
    outDir: '../src/chsmartbulb/webui',
    emptyOutDir: true,
    target: 'es2022',
  },
  server: {
    proxy: { '/ws': { target: daemon, ws: true } },
  },
  test: { include: ['src/**/*.test.ts'] },
})
