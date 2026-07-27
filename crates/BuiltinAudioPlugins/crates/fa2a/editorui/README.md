# FA-2A editor

Embedded editor UI for the `fa2a` built-in plugin. Svelte 5 + Vite, bundled to
a single self-contained `dist/index.html` that `build.rs` embeds into the plugin
library and the native CEF host serves at `mikoplugin://fa2a/index.html`.

## Why this one looks different

The other built-in editors follow the Studio's graphite-and-accent language.
This one is a hardware faceplate — warm enamel, engraved legends, chrome and
bakelite, a lit VU meter. That is deliberate and stays strictly inside the
plugin's own bounds: `DESIGN.md` allows a plugin to carry its own identity
inside its editor, and a leveller whose whole interaction is *two knobs and a
meter* is the case that earns it. It is not a second visual language for the
app, and nothing here should be reused in GPUI chrome.

The interaction contract is still Futureboard's: every control is focusable and
keyboard-drivable, values can be typed, `Home` resets to default, and Shift is
fine adjust.

## Commands

```bash
bun install
bun run build      # svelte-check, then the embedded single-file bundle
bun test           # schema + meter checks
bun run dev        # standalone preview; the meter parks, since nothing feeds it
```

After `bun run build`, rebuild the crate so the new assets are embedded:

```bash
cargo build -p fa2a
```

## The meter is real

The needle is driven by `futureboard.meters`, the host's telemetry frame for
the bound instance, at about 30 Hz. `gainReductionDb` is measured by the DSP's
own optical cell — it is not derived from the input and output levels, because
makeup gain and the dry blend both sit between them and would make any such
estimate wrong.

Carrying it required a field on the shared audio region, so
`BRIDGE_LAYOUT_VERSION` moved to 8. The region is magic- and version-checked, so
a mismatched helper binary is rejected rather than misread.

Two things the meter will not do:

- **Invent a reading.** With no telemetry bound it parks and prints "no signal"
  rather than resting at a plausible number.
- **Show reduction while bypassed.** Power off takes the cell out of the path,
  so the DSP reports zero reduction and the needle says so — but the level
  meters keep working, because setting gain staging before engaging the cell is
  the point.

Ballistics are the VU standard (99 % of a step in 300 ms), interpolated per
animation frame from the real elapsed time so they hold at any frame rate.

## Where authority lives

Rust owns everything that decides what a value *means*:

| Concern | Owner |
| --- | --- |
| parameter ids and wire indices | `../src/ipc.rs` (`UI_PARAM_IDS`) |
| ranges, clamping, defaults | `../src/ipc.rs`, `../src/lib.rs` |
| DSP, metering, persistence | `../src/lib.rs` |
| taper, layout, meter face | `src/params.ts`, `src/meter.ts` |

`src/params.test.ts` parses the real `.rs` files and compares ids, ranges and
the mode order, so a change on either side that is not mirrored fails the tests
rather than shipping a UI that disagrees with the DSP.
