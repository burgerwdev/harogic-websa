import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vite';

export default defineConfig(({ mode }) => {
  // Use checkout bytes, including local changes, rather than pointing dirty
  // builds at an older remote commit. Vitest does not generate build artifacts.
  const legal = mode === 'test' ? { source_id: 'test', archive: 'test.tar.gz' }
    : JSON.parse(execFileSync('python3', [fileURLToPath(new URL('../tools/prepare_legal.py', import.meta.url))], { encoding: 'utf8' }));
  return {
    plugins: [{
      name: 'websa-license-source-links',
      transformIndexHtml: (html: string) => html.replaceAll('__WEBSA_SOURCE_ID__', legal.source_id).replaceAll('__WEBSA_SOURCE_ARCHIVE__', legal.archive),
    }],
    test: {
      environment: 'jsdom',
      setupFiles: ['src/__tests__/setup.ts'],
      // Full-page DOM suites are memory-heavy; bound parallelism on desktops and CI.
      maxWorkers: 2,
    },
    base: '/static/dist/',
    build: {
      outDir: 'dist',
      emptyOutDir: true,
      rolldownOptions: {
        input: {
          index: fileURLToPath(new URL('./index.html', import.meta.url)),
          legal: fileURLToPath(new URL('./legal.html', import.meta.url)),
        },
        output: {
          codeSplitting: {
            // Keep shared translations cacheable without changing synchronous language switching.
            groups: [{ name: 'i18n', test: /[\\/]src[\\/]core[\\/]i18n(?:[\\/]|\.ts$)/ },
              { name: 'countries', test: /[\\/]src[\\/]sdr[\\/]ctyData\.ts$/ }],
          },
        },
      },
    },
    server: {
      port: 5173,
      proxy: {
        '/ws': { target: 'ws://127.0.0.1:8080', ws: true },
        '/api': { target: 'http://127.0.0.1:8080' },
      },
    },
  };
});
