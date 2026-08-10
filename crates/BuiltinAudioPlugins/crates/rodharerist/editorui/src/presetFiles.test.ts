import { describe, expect, test } from "bun:test";
import {
  parsePresetFile,
  presetFileName,
  seedFactoryPresets,
  serializePreset,
} from "./presetFiles";
import { presetsData } from "./data";
import type { RigSnapshot, Snapshot } from "./Editor";
import type { SerializedSnapshotBank } from "./presetFiles";

const SNAPSHOT: RigSnapshot = {
  activeCat: "amp",
  activeModelId: "recto",
  stageModels: {
    dyn: "gate",
    comp: "softknee",
    wah: "cry_wah",
    dist: "rat",
    amp: "recto",
    eq: "parametric",
    mod: "chorus",
    delay: "tape",
    verb: "plate",
    cab: "modern_412",
    comp2: "softknee_2",
    dist2: "screamer_2",
    eq2: "parametric_2",
    mod2: "chorus_2",
    delay2: "tape_2",
  },
  pathOrder: ["dyn", "dist", "amp", "cab"],
  bypassed: { comp: true },
  parameters: { recto: [{ id: "amp_gain", name: "Drive", min: 0, max: 10, val: 9, unit: "" }] },
  globals: { inputTrim: 0, outputTrim: -2, globalBypass: false },
};

describe("preset files", () => {
  test("serialize/parse round-trips", () => {
    const text = serializePreset("U1", "Chug Machine", "amp", SNAPSHOT);
    const parsed = parsePresetFile(text);
    expect(parsed).not.toBeNull();
    expect(parsed?.id).toBe("U1");
    expect(parsed?.name).toBe("Chug Machine");
    expect(parsed?.category).toBe("amp");
    expect(parsed?.snapshot.pathOrder).toEqual(["dyn", "dist", "amp", "cab"]);
    expect(parsed?.snapshot.globals.outputTrim).toBe(-2);
  });

  test("snapshot bank round-trips when present", () => {
    const bank: SerializedSnapshotBank = {
      active: 2,
      slots: Array.from({ length: 8 }, (_, i): Snapshot => ({
        name: `Slot ${i}`,
        bypassed: i === 2 ? { comp: true } : {},
        parameters: { recto: [{ id: "amp_gain", name: "Drive", min: 0, max: 10, val: i, unit: "" }] },
      })),
    };
    const text = serializePreset("U1", "Chug Machine", "amp", SNAPSHOT, bank);
    const parsed = parsePresetFile(text);
    expect(parsed?.snapshots?.active).toBe(2);
    expect(parsed?.snapshots?.slots).toHaveLength(8);
    expect(parsed?.snapshots?.slots[2]?.bypassed).toEqual({ comp: true });
    expect(parsed?.snapshots?.slots[5]?.parameters.recto?.[0]?.val).toBe(5);
  });

  test("omitted snapshot bank stays absent, not a default", () => {
    const text = serializePreset("U1", "Chug Machine", "amp", SNAPSHOT);
    const parsed = parsePresetFile(text);
    expect(parsed?.snapshots).toBeUndefined();
  });

  test("a malformed snapshot bank is dropped, not rejected as an invalid file", () => {
    const file = JSON.parse(serializePreset("U1", "Chug Machine", "amp", SNAPSHOT));
    file.snapshots = { active: "not a number", slots: "not an array" };
    const parsed = parsePresetFile(JSON.stringify(file));
    expect(parsed).not.toBeNull();
    expect(parsed?.snapshots).toBeUndefined();
  });

  test("rejects junk, foreign JSON and future versions", () => {
    expect(parsePresetFile("not json")).toBeNull();
    expect(parsePresetFile("{}")).toBeNull();
    expect(parsePresetFile(JSON.stringify({ format: "other", version: 1 }))).toBeNull();
    const future = JSON.parse(serializePreset("U1", "x", "amp", SNAPSHOT));
    future.version = 99;
    expect(parsePresetFile(JSON.stringify(future))).toBeNull();
  });

  test("file names are native-sanitizer safe", () => {
    // No separators, no dots except the extension, no doubled spaces.
    expect(presetFileName("U1", "My/Weird\\Name: v2.5")).toBe("U1 My Weird Name v2 5.json");
    expect(presetFileName("01A", "Twin Sparkle")).toBe("01A Twin Sparkle.json");
    expect(presetFileName("", "   ")).toBe("Preset.json");
  });

  test("factory seeding writes one valid file per factory preset", () => {
    const written: { fileName: string; content: string }[] = [];
    const count = seedFactoryPresets(
      () => SNAPSHOT,
      (fileName, content) => written.push({ fileName, content }),
    );
    expect(count).toBe(presetsData.length);
    expect(written).toHaveLength(presetsData.length);
    for (const w of written) {
      expect(w.fileName.endsWith(".json")).toBe(true);
      expect(parsePresetFile(w.content)).not.toBeNull();
    }
    // Names are unique.
    expect(new Set(written.map((w) => w.fileName)).size).toBe(written.length);
  });
});
