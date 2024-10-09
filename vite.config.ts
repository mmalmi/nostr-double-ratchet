import { defineConfig } from 'vitest/config';

export default defineConfig({
  build: {
    lib: {
      entry: 'src/index.ts',
      name: 'nostr-double-ratchet',
      // The file name for the generated bundle (entry point of your library)
      fileName: (format) => `nostr-double-ratchet.${format}.js`,
    },
    outDir: 'dist',
  }
});
