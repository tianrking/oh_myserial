import { useMemo } from "react";
import type { WaveSample } from "./protocolUtils";

type WaveformPanelProps = {
  samples: WaveSample[];
  channel: number;
  onChannelChange: (channel: number) => void;
  onClear: () => void;
  protocolLabel: string;
};

const VIEW_WIDTH = 960;
const VIEW_HEIGHT = 220;

function formatValue(value: number | undefined): string {
  return value === undefined || !Number.isFinite(value) ? "—" : value.toPrecision(6);
}

export default function WaveformPanel({
  samples,
  channel,
  onChannelChange,
  onClear,
  protocolLabel,
}: WaveformPanelProps) {
  const channelCount = useMemo(
    () => Math.max(1, samples.reduce((count, sample) => Math.max(count, sample.values.length), 0)),
    [samples],
  );
  const activeChannel = Math.min(channel, channelCount - 1);
  const values = useMemo(
    () => samples.map((sample) => sample.values[activeChannel]).filter((value) => Number.isFinite(value)),
    [activeChannel, samples],
  );
  const { points, min, max } = useMemo(() => {
    if (values.length === 0) return { points: "", min: 0, max: 0 };
    let minimum = Math.min(...values);
    let maximum = Math.max(...values);
    if (minimum === maximum) {
      const padding = Math.max(1, Math.abs(minimum) * 0.05);
      minimum -= padding;
      maximum += padding;
    }
    const span = maximum - minimum;
    const last = Math.max(1, values.length - 1);
    const svgPoints = values
      .map((value, index) => {
        const x = (index / last) * VIEW_WIDTH;
        const y = VIEW_HEIGHT - ((value - minimum) / span) * VIEW_HEIGHT;
        return `${x.toFixed(2)},${y.toFixed(2)}`;
      })
      .join(" ");
    return { points: svgPoints, min: minimum, max: maximum };
  }, [values]);
  const latest = values.at(-1);

  return (
    <section className="panel waveform-panel">
      <div className="log-head">
        <div>
          <h2>波形觀察</h2>
          <p className="hint">
            {protocolLabel} · {values.length} samples · 最新 {formatValue(latest)}
          </p>
        </div>
        <div className="row">
          <label>
            通道
            <select
              value={activeChannel}
              onChange={(event) => onChannelChange(Number(event.target.value))}
            >
              {Array.from({ length: channelCount }, (_, index) => (
                <option key={index} value={index}>
                  CH{index}
                </option>
              ))}
            </select>
          </label>
          <button type="button" className="ghost" onClick={onClear}>
            清空波形
          </button>
        </div>
      </div>
      <div className="waveform-shell">
        {points ? (
          <svg
            className="waveform"
            viewBox={`0 0 ${VIEW_WIDTH} ${VIEW_HEIGHT}`}
            preserveAspectRatio="none"
            role="img"
            aria-label={`CH${activeChannel} 波形，范围 ${formatValue(min)} 到 ${formatValue(max)}`}
          >
            <line x1="0" x2={VIEW_WIDTH} y1={VIEW_HEIGHT / 2} y2={VIEW_HEIGHT / 2} className="waveform-grid" />
            <polyline points={points} className="waveform-line" />
          </svg>
        ) : (
          <p className="muted">選擇 FireWater 或 JustFloat，收到完整幀後會在這裡繪圖。</p>
        )}
      </div>
    </section>
  );
}
