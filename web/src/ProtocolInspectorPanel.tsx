import { useMemo } from "react";
import { bytesToHex } from "./api/wsStream";
import type { ProtocolFrame, StreamProtocol } from "./protocolUtils";
import { protocolLabel } from "./protocolUtils";

type ProtocolInspectorPanelProps = {
  protocol: StreamProtocol;
  frames: ProtocolFrame[];
  onClear: () => void;
};

export default function ProtocolInspectorPanel({
  protocol,
  frames,
  onClear,
}: ProtocolInspectorPanelProps) {
  const recent = useMemo(() => frames.slice(-100), [frames]);
  if (protocol === "raw" || protocol === "firewater" || protocol === "justfloat") return null;

  return (
    <section className="panel protocol-inspector">
      <div className="log-head">
        <div>
          <h2>协议分析</h2>
          <p className="hint">
            {protocolLabel(protocol)} · {frames.length.toLocaleString()} frames · 浏览器本地解析，RX 原始字节不变
          </p>
        </div>
        <button type="button" className="ghost" onClick={onClear}>
          清空分析
        </button>
      </div>
      {recent.length === 0 ? (
        <p className="muted">等待完整帧。跨 WebSocket 分片的半帧会保留在有界缓冲区中。</p>
      ) : (
        <div className="analysis-list">
          {recent.map((frame, index) => (
            <article className="analysis-row" key={`${index}-${frame.summary}`}>
              <div className="analysis-head">
                <span className={`tag ${frame.valid === false ? "analysis-invalid" : "analysis-valid"}`}>
                  {frame.valid === false ? "INVALID" : "FRAME"}
                </span>
                <strong>{frame.summary}</strong>
              </div>
              <div className="analysis-body">
                <code>{bytesToHex(frame.bytes, 256)}</code>
                {frame.fields ? (
                  <span className="analysis-fields">
                    {Object.entries(frame.fields).map(([key, value]) => (
                      <span className="tag" key={key}>
                        {key}={String(value)}
                      </span>
                    ))}
                  </span>
                ) : null}
              </div>
            </article>
          ))}
        </div>
      )}
    </section>
  );
}
