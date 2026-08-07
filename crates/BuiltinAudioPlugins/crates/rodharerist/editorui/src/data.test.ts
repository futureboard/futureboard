import { describe, expect, test } from "bun:test";
import {
  completeParameters,
  models,
  parameterDefaults,
  presetsData,
  stageModelsForPreset,
} from "./data";
import { OUTPUT_TRIM } from "./globals";

describe("factory presets", () => {
  test("use unique stable ids and names", () => {
    expect(new Set(presetsData.map((preset) => preset.id)).size).toBe(
      presetsData.length,
    );
    expect(new Set(presetsData.map((preset) => preset.name)).size).toBe(
      presetsData.length,
    );
  });

  test("reference valid models and valid, non-repeating paths", () => {
    for (const preset of presetsData) {
      const selected = stageModelsForPreset(preset);
      expect(
        models[preset.category].some((model) => model.id === preset.model),
        `${preset.id} focused model`,
      ).toBe(true);

      for (const [category, modelId] of Object.entries(selected)) {
        expect(
          models[category as keyof typeof models].some(
            (model) => model.id === modelId,
          ),
          `${preset.id} ${category} model`,
        ).toBe(true);
      }

      const path = preset.path ?? [];
      expect(new Set(path).size, `${preset.id} duplicate path stage`).toBe(
        path.length,
      );
      expect(path.length, `${preset.id} path size`).toBeLessThanOrEqual(10);
    }
  });

  test("fully specifies every active control with an in-range value", () => {
    for (const preset of presetsData) {
      const selected = stageModelsForPreset(preset);
      const activeParams = (preset.path ?? []).flatMap(
        (category) => parameterDefaults[selected[category]] ?? [],
      );
      const activeIds = new Set(activeParams.map((param) => param.id));

      expect(
        Object.keys(preset.values).filter((id) => !activeIds.has(id)),
        `${preset.id} unused values`,
      ).toEqual([]);

      for (const param of activeParams) {
        const value = preset.values[param.id];
        expect(value, `${preset.id} missing ${param.id}`).toBeNumber();
        expect(value, `${preset.id} ${param.id} minimum`).toBeGreaterThanOrEqual(
          param.min,
        );
        expect(value, `${preset.id} ${param.id} maximum`).toBeLessThanOrEqual(
          param.max,
        );
      }
    }
  });

  // The bank is level-matched through Output Trim (see `Preset.outputTrim`), so
  // a preset that plays a rig but declares no level is one that was never
  // measured — the exact gap that let the bank drift 25 dB apart.
  test("carry a measured output level within the DSP's trim range", () => {
    for (const preset of presetsData) {
      const playsSomething = (preset.path ?? []).length > 0;
      if (!playsSomething) {
        expect(preset.outputTrim, `${preset.id} empty preset`).toBeUndefined();
        continue;
      }
      expect(preset.outputTrim, `${preset.id} missing outputTrim`).toBeNumber();
      expect(preset.outputTrim, `${preset.id} outputTrim minimum`).toBeGreaterThanOrEqual(
        OUTPUT_TRIM.min,
      );
      expect(preset.outputTrim, `${preset.id} outputTrim maximum`).toBeLessThanOrEqual(
        OUTPUT_TRIM.max,
      );
    }
  });

  // The phaser voices take Rate on a cubic law scaled per voice
  // (`rate_hz_from_knob` in `src/dsp/phaser.rs`), so a knob value read as if it
  // were linear in Hz lands near DC: Molam Swirl sat at 2.2, which is 0.05 Hz —
  // one sweep every twenty seconds. Anything below ~0.15 Hz is a stopped LFO,
  // not a slow one.
  test("set phaser-voice rates that actually sweep", () => {
    const RATE_SCALE: Record<string, number> = {
      phaser: 0.85,
      molam_swirl: 0.55,
      phin_vibe: 0.9,
      khaen_swirl: 0.55,
      bi_lam: 0.4,
      isan_jet: 1.35,
    };
    for (const preset of presetsData) {
      if (!(preset.path ?? []).includes("mod")) continue;
      const modModel = stageModelsForPreset(preset).mod;
      const scale = RATE_SCALE[modModel];
      if (scale === undefined) continue; // chorus/flanger/tremolo are linear
      const knob =
        preset.values.chorus_rate ??
        parameterDefaults[modModel]!.find((p) => p.id === "chorus_rate")!.val;
      const t = knob / 10;
      const hz = (0.05 + t * t * t * 3.95) * scale;
      expect(hz, `${preset.id} ${modModel} rate ${knob} = ${hz.toFixed(3)} Hz`).toBeGreaterThan(
        0.15,
      );
    }
  });
});

describe("model parameter schema", () => {
  test("every reverb model exposes its usable controls", () => {
    expect(parameterDefaults.plate!.map((param) => param.id)).toEqual([
      "reverb_decay",
      "reverb_mix",
    ]);
    expect(parameterDefaults.room!.map((param) => param.id)).toEqual([
      "reverb_decay",
      "reverb_mix",
    ]);
    expect(parameterDefaults.hall!.map((param) => param.id)).toEqual([
      "reverb_decay",
      "reverb_mix",
    ]);
    expect(parameterDefaults.shimmer!.map((param) => param.id)).toEqual([
      "reverb_decay",
      "reverb_mix",
      "reverb_shimmer",
    ]);
  });

  test("legacy snapshots are completed before another model is selected", () => {
    const legacy = {
      plate: parameterDefaults.plate!.map((param) => ({ ...param, val: 3 })),
      shimmer: parameterDefaults.shimmer!
        .filter((param) => param.id !== "reverb_shimmer")
        .map((param) => ({ ...param })),
    };
    const completed = completeParameters(legacy);

    expect(completed.room).toEqual(parameterDefaults.room);
    expect(completed.hall).toEqual(parameterDefaults.hall);
    expect(completed.shimmer!.map((param) => param.id)).toEqual([
      "reverb_decay",
      "reverb_mix",
      "reverb_shimmer",
    ]);
    expect(completed.plate!.every((param) => param.val === 3)).toBe(true);
  });
});
