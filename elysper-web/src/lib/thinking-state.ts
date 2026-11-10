/**
 * Is this message's thinking block still being written?
 *
 * A live turn is the one case where a thinking block should be open and
 * scrolling: `isStreaming` is true, the role is the assistant, and the
 * turn's status is `working`. That is exactly the predicate
 * `ChatMessageView` already stamps on the message row as
 * `data-streaming`; keeping it in a pure module means the chat page can
 * assert the rule without a DOM (AGENTS.md §4.2: pure helpers are
 * proven by importing them in the browser).
 */
export function thinkingIsLive(message: {
  isStreaming?: boolean;
  status?: string;
  role: string;
}): boolean {
  return (
    message.isStreaming === true && message.role === 'assistant' && message.status === 'working'
  );
}
