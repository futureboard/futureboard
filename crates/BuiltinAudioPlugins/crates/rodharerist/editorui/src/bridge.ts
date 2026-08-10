// Native bridge for the Rodhareist editor.
//
// The React UI runs inside a CEF view hosted by FutureboardNative. Parameter
// edits travel over the same `__bridge` POST transport as the instance
// binding (see `instanceBridge.ts`): edits are coalesced last-value-per-id
// and flushed once per animation frame as a `futureboard.setParams` batch
// carrying the active instance binding. The ids here map 1:1 to the Rust
// `Dsp::apply_ui_param` contract in `rodharerist/src/dsp/mod.rs` (e.g.
// `drive_gain`, `amp_treble`, `chorus_mix`, plus stage enables `gate_on`…
// and numeric model selects `amp_model`/`drive_model`/`cab_model`/
// `tone_engine`); native resolves them to u32 wire indices from the shared
// `rodharerist::UI_PARAM_IDS` table.
//
// NAM capture loads, clip reset and telemetry ride the same transport:
// loads/clears go out as `__bridge` POSTs, meter/status frames come back as
// native `futureboard.meters` / `futureboard.hostStatus` postMessages (see
// `instanceBridge.ts` for the message shapes).
//
// In a plain browser (e.g. `bun run dev`) no instance is ever bound, so every
// call is a safe no-op — the editor still works for design/preview.

import {
  getActiveParamBinding,
  onNativeMessage,
  onParamBindingReset,
  postLoadIrForBoundInstance,
  postLoadNamCaptureForBoundInstance,
  postSetParams,
} from "./instanceBridge";
import { PATH_SLOTS, baseModelId, stageIndex } from "./data";

export type NamCaptureLoadOptions = {
  /** Display name shown in the editor after a successful load. */
  name: string;
  /** Build two independent models (true stereo width) vs mirror one to both channels. */
  stereo: boolean;
  /** Marks the capture as already modeling amp + cab + mic, for the "Bypass Cab" action. */
  fullRig: boolean;
};

/**
 * One frame of input/output telemetry, mirroring the Rust `MeterFrame` in
 * `rodharerist/src/dsp/mod.rs`. Levels are linear 0..1 amplitudes measured
 * *after* the corresponding trim. Clip flags are sticky until
 * {@link postClearClip}.
 */
export type MeterFrame = {
  inPeak: number;
  inRms: number;
  outPeak: number;
  outRms: number;
  inClip: boolean;
  outClip: boolean;
};

/**
 * Host/engine status for the footer. Every field is optional: the editor shows
 * "—" for anything the host does not report rather than inventing a number.
 */
export type HostStatus = {
  /** Engine sample rate in Hz. */
  sampleRate?: number;
  /** Engine block size in samples. */
  blockSize?: number;
  /** Total plugin latency in samples (NAM and IR block buffering). */
  latencySamples?: number;
  /** Plugin CPU share, 0..1. */
  cpuLoad?: number;
  /** True while the engine is reporting DSP overruns. */
  overload?: boolean;
  /** Channel count the plugin is instantiated with. */
  channels?: number;
};

// --- Coalesced param posting -----------------------------------------------
// Knob drags emit far more edits than the DSP needs; buffer last-value-per-id
// and flush one `futureboard.setParams` batch per animation frame. Map keeps
// insertion order, so multi-id sequences (path_slot_0..6, model + reset)
// arrive at the DSP in the order they were made.

const pendingEdits = new Map<string, number>();
let flushScheduled = false;
let scheduledAnimationFrame: number | null = null;
let scheduledFallback: ReturnType<typeof setTimeout> | null = null;

// CEF can throttle or temporarily stop requestAnimationFrame while DevTools,
// another native window, or a resize/focus transition owns the compositor.
// The timeout is a hard upper bound, not a second flush: whichever callback
// runs first cancels the other one.
const MAX_FLUSH_DELAY_MS = 32;

function cancelScheduledFlush(): void {
  if (
    scheduledAnimationFrame !== null &&
    typeof cancelAnimationFrame === "function"
  ) {
    cancelAnimationFrame(scheduledAnimationFrame);
  }
  if (scheduledFallback !== null) {
    clearTimeout(scheduledFallback);
  }
  scheduledAnimationFrame = null;
  scheduledFallback = null;
  flushScheduled = false;
}

// Edits queued against an instance must die with its binding — a new
// `selectInstance` or an `instanceRemoved` clears the buffer.
onParamBindingReset(() => {
  pendingEdits.clear();
  // Reset the scheduling latch as well. Without this, a throttled RAF from the
  // old binding can leave `flushScheduled=true` indefinitely and prevent every
  // edit for the newly-bound instance from scheduling its own flush.
  cancelScheduledFlush();
});

function flushPendingEdits(): void {
  cancelScheduledFlush();
  if (pendingEdits.size === 0) return;
  const batch = Array.from(pendingEdits, ([id, value]) => ({ id, value }));
  pendingEdits.clear();
  postSetParams(batch);
}

function scheduleFlush(): void {
  if (flushScheduled) return;
  flushScheduled = true;
  if (typeof requestAnimationFrame === "function") {
    scheduledAnimationFrame = requestAnimationFrame(flushPendingEdits);
  }
  scheduledFallback = setTimeout(flushPendingEdits, MAX_FLUSH_DELAY_MS);
}

/**
 * Commit all queued edits synchronously. Discrete UI actions call this after
 * queuing their complete model/bypass/path transaction; continuous knob drags
 * deliberately stay frame-coalesced.
 */
export function flushParamEditsNow(): void {
  flushPendingEdits();
}

/** Backward-compatible test hook. */
export const __flushParamEditsForTest = flushParamEditsNow;

/** Forward a continuous parameter edit (knob) to the native DSP. */
export function postParam(id: string, value: number): void {
  try {
    pendingEdits.set(id, value);
    scheduleFlush();
  } catch {
    // Never let a bridge error break the UI.
  }
}

/**
 * Publish the full Helix path. Every slot is written on every call, missing
 * stages as -1, so removing a block clears its slot rather than leaving the
 * DSP running a stage the path no longer shows.
 *
 * Values are the Rust `StageKind` discriminants, read from `data.ts`'s
 * `stageIndex` rather than a second copy here — one table cannot drift from
 * itself. `gate`/`drive` are the node aliases the DSP uses for `dyn`/`dist`.
 */
export function postPathOrder(path: string[]): void {
  const index: Record<string, number> = {
    ...stageIndex,
    gate: stageIndex.dyn,
    drive: stageIndex.dist,
  };
  for (let i = 0; i < PATH_SLOTS; i++) {
    const cat = path[i];
    const v = cat !== undefined ? (index[cat] ?? -1) : -1;
    postParam(`path_slot_${i}`, v);
  }
}

/** Forward a per-stage bypass toggle. `stage` is a category node id (`amp`…). */
export function postEnabled(stage: string, enabled: boolean): void {
  // Category node ids (`gate`/`drive`/`amp`/`mod`/`delay`/`reverb`/`cab`, plus
  // the `*2` second instances) match the Rust `*_on` param ids exactly.
  postParam(`${stage}_on`, enabled ? 1 : 0);
}

/**
 * Numeric model-select wire values. Each map mirrors the corresponding Rust
 * enum's `ALL` order (`AmpModel`/`DriveModel`/`CabModel` in
 * `rodharerist/src/dsp/mod.rs`) — pinned on the Rust side by
 * `wire::tests::model_select_wire_values_match_editor_ids`.
 */
export const AMP_MODEL_INDEX: Record<string, number> = {
  mandarin: 0,
  plexi: 1,
  twin: 2,
  topboost: 3,
  recto: 4,
  jcm: 5,
  slate: 6,
  bassman: 7,
  boutique: 8,
  invader: 9,
  tweed_combo: 10,
};

export const DRIVE_MODEL_INDEX: Record<string, number> = {
  screamer: 0,
  minotaur: 1,
  rat: 2,
  breaker: 3,
  fuzz: 4,
  centurion: 5,
  ds_one: 6,
  super_drive: 7,
  metal_core: 8,
  tight_rift: 9,
  amber_crunch: 10,
  copper_fuzz: 11,
};

export const CAB_MODEL_INDEX: Record<string, number> = {
  vintage_cab: 0,
  american_2x12: 1,
  tweed_1x12: 2,
  modern_412: 3,
  open_back: 4,
  vintage_212: 5,
  oversized_412: 6,
  bass_cabinet: 7,
  brit_412: 8,
  uber_412: 9,
  slo_412: 10,
  ir: 11,
  modern_212: 12,
  american_1x12: 13,
};

/** `ReverbModel` indices (mirrors Rust `ReverbModel::ALL`). */
export const REVERB_MODEL_INDEX: Record<string, number> = {
  plate: 0,
  room: 1,
  hall: 2,
  shimmer: 3,
};

/** `DelayModel` indices (mirrors Rust `DelayModel::ALL`). */
export const DELAY_MODEL_INDEX: Record<string, number> = {
  tape: 0,
  digital: 1,
  analog: 2,
  ping_pong: 3,
  dual: 4,
};

/** `ModModel` indices (mirrors Rust `ModModel::ALL`). */
export const MOD_MODEL_INDEX: Record<string, number> = {
  chorus: 0,
  phaser: 1,
  flanger: 2,
  tremolo: 3,
  // Append-only: 0-3 are what already-saved projects carry on the wire, so the
  // phaser voices go on the end even though the editor lists them together.
  molam_swirl: 4,
  phin_vibe: 5,
  khaen_swirl: 6,
  bi_lam: 7,
  isan_jet: 8,
  soft_phase: 9,
  wide_vibe: 10,
};

/** `WahModel` indices (mirrors Rust `WahModel::ALL`). */
export const WAH_MODEL_INDEX: Record<string, number> = {
  cry_wah: 0,
  touch_wah: 1,
};

/**
 * `EqModel` indices (mirrors Rust `EqModel::ALL`). `parametric` is the
 * editor's original (and, until the model select was added, only) EQ id —
 * kept as Studio's id rather than renamed, so every existing preset keeps
 * its exact voicing.
 */
export const EQ_MODEL_INDEX: Record<string, number> = {
  parametric: 0,
  vintage_eq: 1,
  modern_eq: 2,
};

/** `ToneEngineKind` indices (Classic=0, NamCapture=1, Bypass=2). */
export const TONE_ENGINE_INDEX = {
  classic: 0,
  nam_capture: 1,
  bypass: 2,
} as const;

/**
 * Forward a model selection within a category (`amp` → `plexi`, …).
 *
 * A second instance arrives as its own node (`drive2`) carrying an editor-side
 * model id (`rat_2`). The suffix exists only so the editor's parameter map can
 * key the two blocks separately — the DSP's model enums are shared, so it is
 * stripped here and the value lands on that block's own `*2_model` param.
 */
export function postModel(category: string, modelId: string): void {
  switch (category) {
    case "drive2": {
      const i = DRIVE_MODEL_INDEX[baseModelId(modelId)];
      if (i !== undefined) postParam("drive2_model", i);
      return;
    }
    case "mod2": {
      const i = MOD_MODEL_INDEX[baseModelId(modelId)];
      if (i !== undefined) postParam("mod2_model", i);
      return;
    }
    case "delay2": {
      const i = DELAY_MODEL_INDEX[baseModelId(modelId)];
      if (i !== undefined) postParam("delay2_model", i);
      return;
    }
    case "eq2": {
      const i = EQ_MODEL_INDEX[baseModelId(modelId)];
      if (i !== undefined) postParam("eq2_model", i);
      return;
    }
    case "amp": {
      // The Tone/Amp slot's special engines ride the `tone_engine` param;
      // a concrete amp model implies Classic (the Rust side resets
      // `tone_engine` itself on `amp_model`).
      if (modelId === "bypass") {
        postParam("tone_engine", TONE_ENGINE_INDEX.bypass);
        return;
      }
      if (modelId === "nam_capture") {
        postParam("tone_engine", TONE_ENGINE_INDEX.nam_capture);
        return;
      }
      const i = AMP_MODEL_INDEX[modelId];
      if (i !== undefined) postParam("amp_model", i);
      return;
    }
    case "dist":
    case "drive": {
      const i = DRIVE_MODEL_INDEX[modelId];
      if (i !== undefined) postParam("drive_model", i);
      return;
    }
    case "cab": {
      const i = CAB_MODEL_INDEX[modelId];
      if (i !== undefined) postParam("cab_model", i);
      return;
    }
    case "mod": {
      const i = MOD_MODEL_INDEX[modelId];
      if (i !== undefined) postParam("mod_model", i);
      return;
    }
    case "wah": {
      const i = WAH_MODEL_INDEX[modelId];
      if (i !== undefined) postParam("wah_model", i);
      return;
    }
    case "eq": {
      const i = EQ_MODEL_INDEX[modelId];
      if (i !== undefined) postParam("eq_model", i);
      return;
    }
    case "verb":
    case "reverb": {
      const i = REVERB_MODEL_INDEX[modelId];
      if (i !== undefined) postParam("reverb_model", i);
      return;
    }
    case "delay": {
      const i = DELAY_MODEL_INDEX[modelId];
      if (i !== undefined) postParam("delay_model", i);
      return;
    }
    default:
      // Single-algorithm stages (gate/comp and their B blocks) have no
      // model select.
      return;
  }
}

/**
 * Load a `.nam` capture into the Tone/Amp slot's NAM engine. `json` is the
 * `.nam` file's raw text content (read client-side via `FileReader`, since
 * the editor runs sandboxed and has no filesystem path access). Travels the
 * `__bridge` POST like params; the result arrives asynchronously as a
 * `futureboard.namCaptureResult` native message (see `instanceBridge.ts`).
 */
export function postLoadNamCapture(json: string, opts: NamCaptureLoadOptions): void {
  postLoadNamCaptureForBoundInstance(json, {
    name: opts.name,
    stereo: opts.stereo,
    fullRig: opts.fullRig,
  });
}

/** One IR load's outcome, mirroring the Rust `IrInfo` in `rodharerist`. */
export type IrLoadResult = {
  ok: boolean;
  name: string;
  error?: string | null;
  frames: number;
  latencySamples: number;
  stereo: boolean;
  truncated: boolean;
};

/**
 * Load a `.wav` impulse response from the plugin's IRs folder into the cabinet
 * slot. Only the file name travels — native reads the bytes itself. The result
 * arrives asynchronously through {@link subscribeIrLoadResult}. Loading and
 * *selecting* the IR cabinet are separate: the DSP keeps the loaded IR ready
 * regardless of which cabinet model is active.
 */
export function postLoadIr(fileName: string): void {
  postLoadIrForBoundInstance(fileName);
}

/** Subscribe to IR load outcomes for the bound instance. */
export function subscribeIrLoadResult(sink: (result: IrLoadResult) => void): () => void {
  return onNativeMessage((msg) => {
    if (msg.type !== "futureboard.irLoadResult") return;
    const binding = getActiveParamBinding();
    if (binding && msg.instanceId !== binding.instanceId) return;
    sink({
      ok: msg.ok,
      name: msg.name,
      error: msg.error,
      frames: msg.frames,
      latencySamples: msg.latencySamples,
      stereo: msg.stereo,
      truncated: msg.truncated,
    });
  });
}

/** Reset the DSP's sticky clip indicators (meter click-to-reset). Routed as a
 * wire param — the DSP treats `clear_clip` as an action, not a value. */
export function postClearClip(): void {
  postParam("clear_clip", 1);
  flushParamEditsNow();
}

/**
 * Subscribe to host telemetry. Frames arrive as native `futureboard.meters` /
 * `futureboard.hostStatus` postMessages for the currently bound instance
 * (~30 Hz / ~1 Hz). Returns an unsubscribe function; with no native host the
 * listener simply never fires and the editor keeps its "no host" state.
 */
export function subscribeTelemetry(sink: {
  onMeters?: (frame: MeterFrame) => void;
  onStatus?: (status: HostStatus) => void;
}): () => void {
  return onNativeMessage((msg) => {
    const binding = getActiveParamBinding();
    if (msg.type === "futureboard.meters") {
      if (binding && msg.instanceId !== binding.instanceId) return;
      sink.onMeters?.({
        inPeak: msg.inPeak,
        inRms: msg.inRms,
        outPeak: msg.outPeak,
        outRms: msg.outRms,
        inClip: msg.inClip,
        outClip: msg.outClip,
      });
    } else if (msg.type === "futureboard.hostStatus") {
      if (binding && msg.instanceId !== binding.instanceId) return;
      sink.onStatus?.({
        sampleRate: msg.sampleRate,
        blockSize: msg.blockSize,
        latencySamples: msg.latencySamples,
        channels: 2,
      });
    }
  });
}

/** Whether a native host bridge is present (useful for conditional UI): true
 * once native has bound this page to a DSP instance. */
export function hasNativeBridge(): boolean {
  return getActiveParamBinding() !== null;
}

/** Whether the host can deliver meter/status telemetry — same condition as
 * `hasNativeBridge` now that telemetry rides the native message channel. */
export function hasTelemetry(): boolean {
  return getActiveParamBinding() !== null;
}
