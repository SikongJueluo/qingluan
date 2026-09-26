import { fileURLToPath } from 'node:url'
import { mergeConfig, defineConfig, configDefaults } from 'vitest/config'
import viteConfig from './vite.config'

export default mergeConfig(
  viteConfig,
  defineConfig({
    test: {
      environment: 'jsdom',
      exclude: [...configDefaults.exclude, 'e2e/**'],
      root: fileURLToPath(new URL('./', import.meta.url)),
      coverage: {
        provider: 'v8',
        reporter: ['text', 'json', 'html'],
        reportsDirectory: './coverage',
        include: ['src/**/*.{ts,vue}'],
        exclude: [
          'src/components/ui/**',
          'src/**/__tests__/**',
          'src/main.ts',
          'src/router/**',
        ],
        // Phase 1: no thresholds (soft report only)
        // Phase 2: uncomment and set gradually
        // thresholds: { lines: 30, functions: 20, branches: 15, statements: 30 },
      },
    },
  }),
)
