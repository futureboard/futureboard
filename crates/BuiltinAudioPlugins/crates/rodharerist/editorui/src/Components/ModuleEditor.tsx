import { useEffect, useState } from "react";
import {
  categories,
  defaultValueFor,
  models,
  type CategoryId,
  type Param,
} from "../data";
import type { NamCaptureLoadOptions } from "../bridge";
import { onNativeMessage } from "../instanceBridge";
import { distanceCm, micTypeLabel, positionLabel } from "../globals";
import { GateMonitor } from "./GateMonitor";
import { Knob } from "./Knob";

/** Lifecycle of the most recent `.nam` load request. */
type NamLoadStatus =
  | { kind: "idle" }
  | { kind: "loading"; name: string }
  | { kind: "loaded"; name: string; receptiveField: number }
  | { kind: "error"; name: string; message: string };

type ModuleEditorProps = {
  activeCat: CategoryId;
  activeModelId: string;
  bypassed: boolean;
  params: Param[];
  onSelectModel: (id: string) => void;
  onToggleBypass: () => void;
  onParamChange: (id: string, value: number) => void;
  onLoadNamCapture: (json: string, opts: NamCaptureLoadOptions) => void;
  onBypassCab: () => void;
};

export function ModuleEditor({
  activeCat,
  activeModelId,
  bypassed,
  params,
  onSelectModel,
  onToggleBypass,
  onParamChange,
  onLoadNamCapture,
  onBypassCab,
}: ModuleEditorProps) {
  const list = models[activeCat] ?? [];
  const model = list.find((m) => m.id === activeModelId) ?? list[0];
  const cat = categories[activeCat];
  const isNamCapture = activeCat === "amp" && activeModelId === "nam_capture";
  const isCabinet = activeCat === "cab";
  const gateThresh =
    activeModelId === "gate"
      ? (params.find((p) => p.id === "gate_thresh") ?? null)
      : null;
  const [namStereo, setNamStereo] = useState(true);
  const [namFullRig, setNamFullRig] = useState(false);
  const [namStatus, setNamStatus] = useState<NamLoadStatus>({ kind: "idle" });

  // Resolve the pending load from the host's async result message.
  useEffect(
    () =>
      onNativeMessage((msg) => {
        if (msg.type !== "futureboard.namCaptureResult") return;
        if (msg.ok) {
          setNamStatus({
            kind: "loaded",
            name: msg.name,
            receptiveField: msg.receptiveField,
          });
        } else {
          setNamStatus({
            kind: "error",
            name: msg.name,
            message: msg.error ?? "load failed",
          });
        }
      }),
    [],
  );

  const paramValue = (id: string, fallback: number) =>
    params.find((p) => p.id === id)?.val ?? fallback;
  const handleCabParamChange = (id: string, value: number) =>
    onParamChange(id, id === "cab_mic_type" ? Math.round(value) : value);

  const handleNamFile = (file: File | undefined) => {
    if (!file) return;
    const reader = new FileReader();
    reader.onload = () => {
      const json = reader.result;
      if (typeof json === "string") {
        const name = file.name.replace(/\.nam$/i, "");
        setNamStatus({ kind: "loading", name });
        onLoadNamCapture(json, {
          name,
          stereo: namStereo,
          fullRig: namFullRig,
        });
      }
    };
    reader.readAsText(file);
  };

  return (
    <section className="editor">
      <div
        className="faceplate"
        style={{ ["--cat-color" as string]: cat.color }}
      >
        <div className="fp-head">
          <div className="fp-identity">
            <span className="fp-stage" style={{ color: cat.color }}>
              {cat.name}
            </span>
            <div className="fp-name">{model?.name ?? "—"}</div>
            <div className="fp-sub">{model?.sub ?? ""}</div>
          </div>
          <button
            className={`bypass${bypassed ? " off" : ""}`}
            onClick={onToggleBypass}
            type="button"
            aria-pressed={!bypassed}
          >
            <span className="led" />
            <span>{bypassed ? "Bypassed" : "Active"}</span>
          </button>
        </div>

        {/* A single-model stage has nothing to choose — the identity header
            already names it, so skip the redundant chip row. Chips are
            compact (name only); the active model's description lives in the
            header, the rest surface theirs as a tooltip. */}
        {list.length > 1 && (
          <div className="model-strip" role="listbox" aria-label="Model">
            {list.map((m) => (
              <button
                key={m.id}
                type="button"
                role="option"
                aria-selected={m.id === activeModelId}
                className={`model-chip${m.id === activeModelId ? " active" : ""}`}
                title={m.sub}
                onClick={() => onSelectModel(m.id)}
              >
                {m.name}
              </button>
            ))}
          </div>
        )}

        {isNamCapture && (
          <div className="nam-capture-controls">
            <label className="nam-file-btn">
              Load .nam Capture…
              <input
                type="file"
                accept=".nam"
                onChange={(e) => handleNamFile(e.target.files?.[0])}
              />
            </label>
            <label className="nam-check">
              <input
                type="checkbox"
                checked={namStereo}
                onChange={(e) => setNamStereo(e.target.checked)}
              />
              Stereo (two independent models)
            </label>
            <label className="nam-check">
              <input
                type="checkbox"
                checked={namFullRig}
                onChange={(e) => setNamFullRig(e.target.checked)}
              />
              Full Rig capture (amp + cab + mic)
            </label>
            {namFullRig && (
              <button type="button" className="nam-bypass-cab" onClick={onBypassCab}>
                Bypass Cab
              </button>
            )}
            {namStatus.kind !== "idle" && (
              <div
                className={`nam-load-status ${namStatus.kind}`}
                role="status"
                aria-live="polite"
              >
                {namStatus.kind === "loading" && `Loading “${namStatus.name}”…`}
                {namStatus.kind === "loaded" &&
                  `Loaded “${namStatus.name}” (${namStatus.receptiveField} sample receptive field)`}
                {namStatus.kind === "error" &&
                  `“${namStatus.name}” failed: ${namStatus.message}`}
              </div>
            )}
          </div>
        )}

        {isCabinet ? (
          // Mic placement is edited with the same knobs as every other module;
          // the readout translates the two parameters into the terms an engineer
          // thinks in (axis position, centimetres).
          <div className="cab-inspector">
            <div className="param-bank">
              {params.map((p) => (
                <Knob
                  key={p.id}
                  id={p.id}
                  name={p.name}
                  min={p.min}
                  max={p.max}
                  value={p.val}
                  unit={p.unit}
                  defaultValue={defaultValueFor(activeModelId, p.id)}
                  onChange={handleCabParamChange}
                />
              ))}
            </div>
            <div className="cab-readout">
              <span>
                <b>{micTypeLabel(paramValue("cab_mic_type", 0))}</b>
              </span>
              <span>
                <b>{positionLabel(paramValue("cab_mic", 20))}</b>{" "}
                {paramValue("cab_mic", 20).toFixed(0)}%
              </span>
              <span>{distanceCm(paramValue("cab_dist", 40)).toFixed(1)} cm</span>
            </div>
            <p className="inspector-note">
              Position is measured from the speaker centre; distance is shown on a
              0–30 cm scale. Capsule type, cone position, proximity, air absorption
              and the first room reflection are modelled independently.
            </p>
          </div>
        ) : (
          <div className="param-bank">
            {params.map((p) => (
              <Knob
                key={p.id}
                id={p.id}
                name={p.name}
                min={p.min}
                max={p.max}
                value={p.val}
                unit={p.unit}
                defaultValue={defaultValueFor(activeModelId, p.id)}
                onChange={onParamChange}
              />
            ))}
            {gateThresh && (
              <GateMonitor
                paramId={gateThresh.id}
                threshold={gateThresh.val}
                min={gateThresh.min}
                max={gateThresh.max}
                onChange={onParamChange}
              />
            )}
          </div>
        )}
      </div>
    </section>
  );
}
