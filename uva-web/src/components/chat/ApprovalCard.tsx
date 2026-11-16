/**
 * The approval card for a suspended run.
 *
 * A run stops before dispatching a call the tool classified as
 * high-risk; the durable `approval_asked` rows are what the `follow`
 * baseline's `pending_interrupts` reports. The card is bound to the
 * held call (the timeline renders it inside that call's tool block when
 * the block is in the window, and the page falls back to a free-standing
 * card when it is not), because "approve this" is meaningless without
 * the call it applies to.
 *
 * A decision is one per interrupt — the resume must cover every open
 * one, so **every** card's submit is gated on the session-wide open set
 * (a card that holds only the calls outside the window must not submit
 * while a call inside it is still undecided). A rejection is not
 * silent: the harness commits an `is_error` tool result the model sees,
 * so the refused call reads in the transcript as the failure it was.
 *
 * "Approve once" is exactly that. There is no per-click permanent
 * grant; a deployment-scope grant can only come from server
 * configuration.
 *
 * A failed resume is actionable, not decorative: `config_drift` offers
 * a retry that accepts the changed envelope, and `resume_conflict`
 * offers a reconnect that re-reads the baseline (and therefore a fresh
 * token). `resume_no_snapshot` offers neither: that round cannot be
 * resumed, so the way forward is a new session.
 */
import type { JSX } from 'react';
import type { ApprovalRequest, ResumeStatus } from '../../api/types';
import { Button } from '../ui/Button';

export interface ApprovalCardProps {
  interrupts: ReadonlyArray<ApprovalRequest>;
  /** Chosen status per interrupt id, until the resume is submitted. */
  decisions: Readonly<Record<string, ResumeStatus>>;
  /** Interrupts the operator approved for the rest of the session. */
  grants: Readonly<Record<string, boolean>>;
  /** Approve this call for the rest of the session. */
  onGrant: (id: string) => void;
  busy: boolean;
  error: string | null;
  /** The `RpcError.code` behind `error`, when it has one. */
  errorCode: string | null;
  /** True when every open interrupt in the session has a decision. */
  ready: boolean;
  /** Open interrupts (session-wide) that still lack a decision. */
  undecided: number;
  onDecide: (id: string, status: ResumeStatus) => void;
  onReset: (id: string) => void;
  onSubmit: () => void;
  /** Retry the resume accepting a changed request envelope. */
  onRetryDrift?: () => void;
  /** Re-open the follow stream, which re-reads the baseline. */
  onReconnect?: () => void;
}

const DECIDED_LABEL: Record<ResumeStatus, string> = {
  resolved: '已批准一次',
  rejected: '已拒绝',
  cancelled: '已取消',
};

export function ApprovalCard({
  interrupts,
  decisions,
  grants,
  onGrant,
  busy,
  error,
  errorCode,
  ready,
  undecided,
  onDecide,
  onReset,
  onSubmit,
  onRetryDrift,
  onReconnect,
}: ApprovalCardProps): JSX.Element {
  return (
    <div
      className="nt-chat__approval"
      data-testid="approval-card"
      role="group"
      aria-label="等待审批"
    >
      <div className="nt-chat__approval-head">
        <span aria-hidden>🔐</span>
        <span className="nt-chat__approval-title">需要审批</span>
        <span className="nt-chat__approval-count">{interrupts.length} 个高风险调用</span>
      </div>
      <ul className="nt-chat__approval-list">
        {interrupts.map((interrupt) => {
          const decided = decisions[interrupt.id];
          return (
            <li key={interrupt.id} className="nt-chat__approval-item" data-testid="approval-item">
              <div className="nt-chat__approval-item-head">
                <code className="nt-chat__approval-tool">{interrupt.tool}</code>
                <span
                  className={`nt-chat__approval-risk nt-chat__approval-risk--${interrupt.risk}`}
                >
                  {interrupt.risk}
                </span>
                <span className="nt-chat__approval-agent">{interrupt.agent}</span>
              </div>
              <div className="nt-chat__approval-hash" title={interrupt.arguments_hash}>
                参数摘要 <code>{interrupt.arguments_hash.slice(0, 16)}</code>
              </div>
              {decided !== undefined ? (
                <div className="nt-chat__approval-decided">
                  <span
                    className={`nt-chat__approval-decision nt-chat__approval-decision--${decided}`}
                    data-testid={`approval-decided-${decided}`}
                  >
                    {decided === 'resolved' && grants[interrupt.id]
                      ? '已批准本会话'
                      : DECIDED_LABEL[decided]}
                  </span>
                  <Button
                    size="1"
                    variant="ghost"
                    color="gray"
                    disabled={busy}
                    onClick={() => onReset(interrupt.id)}
                    data-testid="approval-reset"
                    aria-label="Change this decision"
                  >
                    更改
                  </Button>
                </div>
              ) : (
                <div className="nt-chat__approval-actions">
                  <Button
                    size="1"
                    variant="solid"
                    color="green"
                    disabled={busy}
                    onClick={() => onDecide(interrupt.id, 'resolved')}
                    data-testid="approval-approve"
                  >
                    批准一次
                  </Button>
                  <Button
                    size="1"
                    variant="soft"
                    color="green"
                    disabled={busy}
                    onClick={() => onGrant(interrupt.id)}
                    data-testid="approval-grant-session"
                  >
                    批准本会话
                  </Button>
                  <Button
                    size="1"
                    variant="soft"
                    color="red"
                    disabled={busy}
                    onClick={() => onDecide(interrupt.id, 'rejected')}
                    data-testid="approval-reject"
                  >
                    拒绝
                  </Button>
                </div>
              )}
            </li>
          );
        })}
      </ul>
      <p className="nt-chat__approval-note">
        拒绝会把工具错误结果写回模型可见的历史；批准一次只对本次调用生效，批准本会话会写一条可重放的授权行。
      </p>
      {error && (
        <div className="nt-chat__approval-error" role="alert">
          {error}
          {errorCode === 'config_drift' && onRetryDrift && (
            <Button
              size="1"
              variant="soft"
              color="blue"
              disabled={busy}
              onClick={onRetryDrift}
              data-testid="approval-retry-drift"
            >
              接受配置漂移并重试
            </Button>
          )}
          {errorCode === 'resume_conflict' && onReconnect && (
            <Button
              size="1"
              variant="soft"
              color="blue"
              disabled={busy}
              onClick={onReconnect}
              data-testid="approval-reconnect"
            >
              重新连接并同步
            </Button>
          )}
        </div>
      )}
      <div className="nt-chat__approval-footer">
        <Button
          size="1"
          variant="solid"
          color="blue"
          disabled={!ready || busy}
          onClick={onSubmit}
          data-testid="approval-submit"
        >
          {busy ? '恢复中…' : ready ? '提交决定并恢复' : `还有 ${undecided} 个未决定`}
        </Button>
      </div>
    </div>
  );
}
