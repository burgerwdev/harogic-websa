import { defineConfig } from 'vite';

export default defineConfig({
  test: {
    environment: 'jsdom',
    setupFiles: ['src/__tests__/setup.ts'],
  },
  base: '/static/modern/dist/',
  build: {
    outDir: 'dist',
    emptyOutDir: true,
  },
  server: {
    port: 5173,
    proxy: {
      '/ws': { target: 'ws://127.0.0.1:8080', ws: true },
      '/api': { target: 'http://127.0.0.1:8080' },
    },
  },
});
