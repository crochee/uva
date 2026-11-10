/**
 * The bash-style prompt in front of the input: `❯ <工作区路径> `.
 *
 * The workspace is the constant default workspace (D35): the server has
 * one `workspace_root` and exposes no path RPC — `GET /version` answers
 * build identity, and `SessionSummary` has no workspace field — so
 * inventing a path here would be wrong. When the workspace surface
 * lands, this prop becomes the real value and nothing else changes.
 *
 * The optional `context` marker names the agent a `/chat/:id/agent/:name`
 * route pinned, so the prompt says who will answer.
 */
export function PromptPrefix({ workspace, context }: { workspace: string; context?: string }) {
  return (
    <div className="nt-chat__prompt-prefix" data-testid="prompt-prefix">
      <span aria-hidden className="nt-chat__prompt-caret">
        ❯
      </span>
      <span className="nt-chat__prompt-workspace">{workspace}</span>
      {context ? <span className="nt-chat__prompt-context">{`@${context}`}</span> : null}
    </div>
  );
}
