/// <reference types="vitest" />
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const __dirname = path.dirname(fileURLToPath(import.meta.url))

// https://vitejs.dev/config/
export default defineConfig({
  plugins: [react(), tailwindcss()],
  // frontend/ contains index.html and React sources
  root: 'frontend',
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './frontend/src'),
    },
  },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
  },
  server: {
    port: 1420,
    strictPort: true,
  },
  envPrefix: ['VITE_', 'TAURI_ENV_'],
  test: {
    environment: 'jsdom',
    globals: true,
    include: ['src/**/*.test.{ts,tsx}', 'src/**/__tests__/**/*.{ts,tsx}'],
  },
})
