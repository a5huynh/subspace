import tailwindcss from '@tailwindcss/vite'
import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

// The dev server proxies the API (REST + WebSocket) to subspace-server, so the
// console talks to same-origin `/api` and needs no CORS. Override the target
// with SUBSPACE_API, e.g. `SUBSPACE_API=http://127.0.0.1:8080 npm run dev`.
const apiTarget = process.env.SUBSPACE_API ?? 'http://127.0.0.1:7700'

// https://vite.dev/config/
export default defineConfig({
  plugins: [react(), tailwindcss()],
  server: {
    proxy: {
      '/api': { target: apiTarget, ws: true },
    },
  },
})
