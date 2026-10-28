/**
 * One compaction, rendered in place in the timeline.
 *
 * The durable `compaction` row replaces a span of older surface
 * messages in what the model is sent, but it does not delete them from
 * the log. The transcript therefore shows both: the turns the reader
 * remembers, and this marker saying that a span was swapped for the
 * summary below on the next request. Without it, older turns quietly
 * stop mattering and the reader has no way to learn why.
 */
import type { JSX } from 'react';
import type { CompactionInfo } from '../../api/chat-message';

export function CompactionMarker({ info }: { info: CompactionInfo }): JSX.Element {
  const surface =
    info.start !== undefined && info.end !== undefined ? `#${info.start}–#${info.end}` : null;
  const durable =
    info.firstSeq !== undefined && info.lastSeq !== undefined
      ? `seq ${info.firstSeq}–${info.lastSeq}`
      : null;
  return (
    <div className="nt-chat__compaction" data-testid="compaction-marker">
      <div className="nt-chat__compaction-head">
        <span aria-hidden>🗜️</span>
        <span className="nt-chat__compaction-title">历史已压缩</span>
        {surface && <span className="nt-chat__compaction-range">范围 {surface}</span>}
        {info.shadowedMessages !== undefined && <span>{info.shadowedMessages} 条消息</span>}
        {info.shadowedTokens !== undefined && (
          <span>{info.shadowedTokens.toLocaleString()} tokens</span>
        )}
        {durable && <span className="nt-chat__compaction-seq">{durable}</span>}
      </div>
      {info.summary && (
        <pre className="nt-chat__compaction-summary" data-testid="compaction-summary">
          {info.summary}
        </pre>
      )}
    </div>
  );
}
