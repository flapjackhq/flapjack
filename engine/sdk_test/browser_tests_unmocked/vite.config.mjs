import { defineConfig } from 'vite';
import { fileURLToPath } from 'node:url';

export default defineConfig({
  // The fixture app is test-owned and intentionally separate from the product dashboard.
  root: fileURLToPath(new URL('./app', import.meta.url)),
  server: {
    strictPort: true,
  },
});
