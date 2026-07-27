import { describe, expect, test } from "bun:test";
import {
  models,
  parameterDefaults,
  presetsData,
  stageModelsForPreset,
} from "./data";

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
});
