import { svelte } from '@sveltejs/vite-plugin-svelte'
import tailwindcss from '@tailwindcss/vite'
import { defineConfig } from 'vitest/config'

// `npm run dev` serves the page with hot reload and passes /ws on to a running
// `chsmartbulb daemon --web 8378`; `npm run build` writes the bundle the daemon serves.
const daemon = process.env.CHSMARTBULB_WEB ?? 'ws://127.0.0.1:8378'

// `--mode app` builds the same page for the desktop and mobile app (app/), which talks
// to the service inside it over Tauri's IPC; a phone reaches the dev server at TAURI_DEV_HOST.
const host = process.env.TAURI_DEV_HOST

export default defineConfig(({ mode }) => ({
  plugins: [svelte(), tailwindcss()],
  build: {
    outDir: mode === 'app' ? 'dist-app' : '../src/chsmartbulb/webui',
    emptyOutDir: true,
    target: 'es2022',
  },
  clearScreen: mode !== 'app',
  server:
    mode === 'app'
      ? { host: host || false, port: 5173, strictPort: true, hmr: host ? { protocol: 'ws', host, port: 5174 } : undefined }
      : { proxy: { '/ws': { target: daemon, ws: true } } },
  test: { include: ['src/**/*.test.ts'] },
}))
