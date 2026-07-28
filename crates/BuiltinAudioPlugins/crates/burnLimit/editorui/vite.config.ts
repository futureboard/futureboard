import { defineConfig } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'
import { viteSingleFile } from 'vite-plugin-singlefile'
import { fileURLToPath } from 'node:url'

// The editor is embedded into the plugin library as a single, fully
// self-contained `index.html` (JS/CSS inlined). It is served to CEF through the
// `mikoplugin://burnlimit/index.html` custom scheme, so there must be no
// sibling asset requests and no network fetch at runtime.
export default defineConfig({
  // Pin the root to this config's own URL: the checkout may be reached through
  // more than one path, and a relative root makes Vite emit index.html as an
  // absolute asset reference the custom scheme cannot resolve.
  root: fileURLToPath(new URL('.', import.meta.url)),
  plugins: [svelte(), viteSingleFile()],
  build: {
    target: 'chrome120',
    assetsInlineLimit: Infinity,
    cssCodeSplit: false,
  },
})
