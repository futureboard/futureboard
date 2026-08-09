import tailwindcss from '@tailwindcss/vite'
import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'
import { viteSingleFile } from 'vite-plugin-singlefile'

// The editor ships as one self-contained HTML file embedded by `builtin_ui_embed`
// and served over the `mikoplugin:` scheme, so every asset — fonts included —
// must inline. No dev server or network origin exists at runtime.
export default defineConfig({
  plugins: [react(), tailwindcss(), viteSingleFile()],
  build: {
    target: 'es2022',
    assetsInlineLimit: 100_000_000,
    cssCodeSplit: false,
  },
})
