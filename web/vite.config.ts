import { defineConfig } from 'vitest/config'
import react from '@vitejs/plugin-react'
import { tanstackRouter } from '@tanstack/router-plugin/vite'
import { fileURLToPath } from 'node:url'
import { mkdirSync, writeFileSync } from 'node:fs'

// Keeps web/dist/.gitkeep (tracked, so the dir survives a fresh clone and
// `include_dir!("web/dist")` always has a target). vite's emptyOutDir would
// otherwise delete it on every build.
function keepDistPlaceholder() {
  return {
    name: 'keep-dist-placeholder',
    apply: 'build' as const,
    closeBundle() {
      const keep = fileURLToPath(new URL('./dist/.gitkeep', import.meta.url))
      mkdirSync(fileURLToPath(new URL('./dist', import.meta.url)), { recursive: true })
      writeFileSync(keep, '')
    },
  }
}

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
    keepDistPlaceholder(),
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
