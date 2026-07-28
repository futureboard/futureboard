# BurnLimit editor

Embedded editor for the `burnlimit` built-in. Svelte 5 + Vite singlefile,
served at `mikoplugin://burnlimit/index.html`.

## Character

Stage-first maximizer: steel input body, vermillion GR from the top, amber
ceiling / peak marks, vertical Gain fader, dense bottom strip. Identity comes
from the display — not glow or ornament.

```bash
bun install
bun run build
bun test
```

`cargo build -p burnlimit` also performs a frozen Bun install and builds the
single-file editor into Cargo's `OUT_DIR` before embedding it. A clean checkout
therefore does not need a pre-existing `editorui/dist` directory.
