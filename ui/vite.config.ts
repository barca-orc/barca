import { defineConfig } from 'vite'
import { fileURLToPath, URL } from 'node:url'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'

// barca serve binds 127.0.0.1:8274 by default and serves the built UI at /ui/.
// Dev mirrors that: Vite serves the app at /ui/ and proxies every other path
// (the API) to barca serve, so the frontend stays same-origin with no CORS.
const BARCA_SERVE = 'http://127.0.0.1:8274'

// https://vite.dev/config/
export default defineConfig(({ command }) => ({
  // Relative asset URLs in the build, so the UI works under any reverse-proxy
  // prefix (nginx at /barca/ → /barca/ui/). Dev serves at /ui/ like barca does.
  base: command === 'build' ? './' : '/ui/',
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url)),
    },
  },
  // Playwright specs in e2e/ run with `pnpm test:e2e`, not vitest.
  test: { exclude: ['e2e/**', 'node_modules/**'] },
  server: {
    proxy: {
      // Everything outside /ui/ is the API.
      '^/(?!ui(/|$))': {
        target: BARCA_SERVE,
        changeOrigin: true,
      },
    },
  },
}))
