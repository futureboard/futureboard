import { useEffect, useState } from "react";
import { hasTelemetry } from "../bridge";
import {
  getSource,
  getStatus,
  subscribeSource,
  subscribeStatus,
  type HostStatus,
  type MeterSource,
} from "../state/meters";

type FooterProps = {
  globalBypass: boolean;
};

const DASH = "—";

function formatSampleRate(hz: number | undefined): string {
  if (!hz) return DASH;
  return `${(hz / 1000).toFixed(hz % 1000 === 0 ? 0 : 1)} kHz`;
}

function formatLatency(samples: number | undefined, sampleRate: number | undefined): string {
  if (samples === undefined) return DASH;
  if (samples === 0) return "0 ms";
  if (!sampleRate) return `${samples} smp`;
  return `${((samples / sampleRate) * 1000).toFixed(1)} ms`;
}

function formatCpu(load: number | undefined): string {
  if (load === undefined) return DASH;
  return `CPU ${Math.round(load * 100)}%`;
}

/**
 * Compact system status.
 *
 * Every engine-supplied figure renders as `—` until the host actually reports
 * it. Latency in particular is never shown as zero on the assumption that the
 * chain is zero-latency: the NAM and IR engines both buffer a block, and the
 * host is the only thing that knows the total.
 */
export function Footer({ globalBypass }: FooterProps) {
  const [status, setStatus] = useState<HostStatus>(() => getStatus());
  const [source, setSource] = useState<MeterSource>(() => getSource());

  useEffect(() => subscribeStatus(setStatus), []);
  useEffect(() => subscribeSource(setSource), []);

  const cells = [
    globalBypass ? "BYPASSED" : "ACTIVE",
    status.channels === 1 ? "MONO" : status.channels === 2 ? "STEREO" : DASH,
    formatSampleRate(status.sampleRate),
    status.blockSize ? `${status.blockSize} samples` : DASH,
    formatLatency(status.latencySamples, status.sampleRate),
    formatCpu(status.cpuLoad),
  ];

  return (
    <footer className="footer">
      <div className="footer-status">
        <span
          className={`dsp${globalBypass ? " off" : ""}${status.overload ? " overload" : ""}`}
          aria-hidden
        >
          &#9679;
        </span>
        {cells.map((cell, i) => (
          <span key={i} className={cell === DASH ? "cell muted" : "cell"}>
            {cell}
          </span>
        ))}
        {status.overload && <span className="cell overload-text">DSP OVERLOAD</span>}
      </div>

      <div className="footer-right">
        {!hasTelemetry() && (
          <span
            className="cell muted"
            title="No host telemetry: meters and system figures are unavailable outside the plugin host"
          >
            {source === "preview" ? "PREVIEW METERS" : "NO HOST TELEMETRY"}
          </span>
        )}
        <span>Rodhareist Native</span>
      </div>
    </footer>
  );
}
