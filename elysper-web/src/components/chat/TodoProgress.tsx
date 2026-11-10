/**
 * The todo strip above the composer (design §6.1).
 *
 * It shows the plan the run is executing — the last `TodoWrite` list —
 * as `n/m 完成` plus one row per item. Two rules are load-bearing:
 *
 * - **no todos, no DOM**: `null` (or an empty list) returns `null`
 *   before any element is created, so the composer never reserves an
 *   empty box (review focus 3);
 * - the denominator is `items.length`, not a separately tracked total,
 *   so the count cannot disagree with the rows next to it.
 *
 * The data is the same `panelTodos` the side panel renders, so the two
 * surfaces cannot disagree about the run.
 */
import type { PanelTodoState } from '../../lib/session-panel-data';

const MARK: Record<string, string> = {
  completed: '✔',
  in_progress: '▸',
  cancelled: '✕',
  pending: '·',
};

export function TodoProgress({ todos }: { todos: PanelTodoState | null }) {
  if (todos === null || todos.items.length === 0) return null;
  return (
    <div className="nt-chat__todo-strip" data-testid="todo-progress">
      <div className="nt-chat__todo-head">
        <span className="nt-chat__todo-tag">待办</span>
        <span className="nt-chat__todo-plan">这一轮的计划</span>
        <span className="nt-chat__todo-count" data-testid="todo-count">
          {`${todos.completed}/${todos.items.length} 完成`}
        </span>
      </div>
      <ul className="nt-chat__todo-items">
        {todos.items.map((item) => (
          <li
            key={item.id}
            className={`nt-chat__todo-item nt-chat__todo-item--${item.status}`}
            data-testid={`todo-item-${item.id}`}
          >
            <span aria-hidden className="nt-chat__todo-mark">
              {MARK[item.status] ?? MARK.pending}
            </span>
            <span className="nt-chat__todo-text">{item.content}</span>
          </li>
        ))}
      </ul>
    </div>
  );
}
