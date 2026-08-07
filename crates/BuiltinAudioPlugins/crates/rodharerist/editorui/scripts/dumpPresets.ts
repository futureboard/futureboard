// Emit the factory presets as the flat, DSP-facing JSON that
// `examples/preset_audit.rs` renders, so the audit measures the same values the
// editor loads instead of a second copy that would drift.
//
//   bun run presets:json > presets.json
//   cargo run -p rodharerist --example preset_audit --release -- presets.json
//
// Resolution mirrors `Editor.tsx`'s `factorySnapshot`: complete the per-stage
// model selection, overlay the preset's values onto the full model schema, and
// carry the preset's own output level.

import {
  chainOrder,
  parametersForPreset,
  presetsData,
  stageModelsForPreset,
  type CategoryId,
} from "../src/data";

const resolved = presetsData.map((preset) => {
  const stageModels = stageModelsForPreset(preset);
  const parameters = parametersForPreset(preset);
  const path = preset.path ?? [];
  const bypassed = new Set(preset.bypassed ?? []);

  // Only the model selected in a stage that is actually in the path can affect
  // the sound, so only those values are emitted.
  const values: Record<string, number> = {};
  for (const category of path) {
    for (const param of parameters[stageModels[category]] ?? []) {
      values[param.id] = param.val;
    }
  }

  return {
    id: preset.id,
    name: preset.name,
    category: preset.category,
    path,
    enabled: Object.fromEntries(
      chainOrder.map((category: CategoryId) => [category, !bypassed.has(category)]),
    ),
    stageModels,
    values,
    outputTrim: preset.outputTrim ?? 0,
  };
});

console.log(JSON.stringify(resolved, null, 2));
