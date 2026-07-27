# FA-76 editor

Embedded editor UI for the `fa76` built-in plugin. Svelte 5 + Vite, bundled to
a single `dist/index.html` by `vite-plugin-singlefile`, then embedded into the
library and the native CEF host serves at `mikoplugin://fa76/index.html`.

## Character

Black anodized FET / 1176 faceplate — blue VU, aluminum knobs, ratio
pushbuttons. Distinct from FA-2A's warm optical leveling-amp look.

## Develop

```bash
bun install
bun run dev
bun run build
bun test
```

After a production build:

```bash
cargo build -p fa76
```
