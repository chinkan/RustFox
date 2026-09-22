import { defineConfig } from 'vitest/config'
import react from '@vitejs/plugin-react'
import { tanstackRouter } from '@tanstack/router-plugin/vite'

// NOTE: the TanStack Router plugin MUST come before react() so that route
// generation + code-splitting happen before the React transform.
export default defineConfig({
  plugins: [
    tanstackRouter({
      target: 'react',
      routesDirectory: './src/routes',
      generatedRouteTree: './src/routeTree.gen.ts',
    }),
    react(),
  ],
  server: {
    port: 5173,
    // Proxy to the RustFox Axum API. In production Axum serves the built
    // SPA from dist/ and owns /api/* itself, so no proxy is needed.
    proxy: {
      '/api': {
        target: process.env.RUSTFOX_API ?? 'http://127.0.0.1:8090',
        changeOrigin: true,
      },
    },
  },
  build: {
    outDir: 'dist',
    sourcemap: true,
  },
  test: {
    environment: 'jsdom',
    globals: true,
    include: ['src/**/*.test.{ts,tsx}'],
  },
})
