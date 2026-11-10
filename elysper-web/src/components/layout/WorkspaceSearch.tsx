/**
 * The group-header cross-domain search (🔍).
 *
 * One box, one engine: `GET /api/v1/search` — the same `search/all`
 * method the agent's `search` tool calls, so the sidebar and the model
 * agree on what exists. Hits land on their new homes: sessions open in
 * the conversation; capability domains open the matching settings
 * section (the old catalogue pages are gone, D34). Memory hits carry
 * their preview inline because there is no memory page.
 *
 * Keyboard: `↑`/`↓` (or `j`/`k`, `n`/`N`) move the highlight, `Enter`
 * opens, `Esc` closes. The list takes focus when the user arrows down
 * out of the input, which is what makes single-letter navigation
 * usable — and why the input itself stays a normal text field.
 */
import { useCallback, useEffect, useRef, useState, type KeyboardEvent } from 'react';
import { useNavigate } from 'react-router-dom';
import { api } from '../../api/client';
import type { List, SearchHit } from '../../api/types';

/** Hit cap; the backend clamps to the same ceiling. */
const SEARCH_LIMIT = 20;

/** Where a hit lands. `null` means "no page": the row still renders
 *  (with its preview) but is not a navigation. */
function pathForHit(hit: SearchHit): string | null {
  switch (hit.domain) {
    case 'session':
      return `/chat/${encodeURIComponent(hit.id)}`;
    case 'agent':
      return '/settings/agents';
    case 'skill':
      return '/settings/skills';
    case 'tool':
      return '/settings/plugins';
    case 'mcp':
      return '/settings/mcp';
    default:
      return null;
  }
}

const DOMAIN_LABEL: Record<string, string> = {
  session: '会话',
  agent: '智能体',
  skill: '技能',
  tool: '工具',
  mcp: 'MCP',
  memory: '记忆',
};

export function WorkspaceSearch({ collapsed }: { collapsed: boolean }) {
  const navigate = useNavigate();
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState('');
  const [hits, setHits] = useState<SearchHit[]>([]);
  const [cursor, setCursor] = useState(0);
  const [searching, setSearching] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const listRef = useRef<HTMLUListElement | null>(null);
  const abortRef = useRef<AbortController | null>(null);

  // `/` and the group-header button both open the same panel.
  useEffect(() => {
    function onOpen(): void {
      setOpen(true);
    }
    window.addEventListener('elysper:search-open', onOpen);
    return () => window.removeEventListener('elysper:search-open', onOpen);
  }, []);

  useEffect(() => {
    if (!open) return;
    const input = inputRef.current;
    if (input) {
      input.focus();
      input.select();
    }
  }, [open]);

  const trimmed = query.trim();
  useEffect(() => {
    if (!open) return;
    if (!trimmed) {
      if (abortRef.current) abortRef.current.abort();
      setHits([]);
      setError(null);
      setSearching(false);
      return;
    }
    const timer = setTimeout(() => {
      if (abortRef.current) abortRef.current.abort();
      const controller = new AbortController();
      abortRef.current = controller;
      setSearching(true);
      api
        .get<List<SearchHit>>(
          `/api/v1/search?q=${encodeURIComponent(trimmed)}&limit=${SEARCH_LIMIT}`,
          controller.signal,
        )
        .then((response) => {
          if (abortRef.current !== controller) return;
          setHits(response.data);
          setCursor(0);
          setError(null);
        })
        .catch((cause: Error) => {
          if (cause.name === 'AbortError') return;
          if (abortRef.current !== controller) return;
          setError(cause.message);
          setHits([]);
        })
        .finally(() => {
          if (abortRef.current === controller) setSearching(false);
        });
    }, 200);
    return () => clearTimeout(timer);
  }, [trimmed, open]);

  useEffect(() => {
    return () => {
      if (abortRef.current) abortRef.current.abort();
    };
  }, []);

  const close = useCallback(() => {
    setOpen(false);
    setQuery('');
    setHits([]);
    setError(null);
  }, []);

  const move = (delta: number): void => {
    if (hits.length === 0) return;
    setCursor((current) => (current + delta + hits.length) % hits.length);
  };

  const openHit = (hit: SearchHit | undefined): void => {
    if (!hit) return;
    const path = pathForHit(hit);
    if (!path) return;
    close();
    navigate(path);
  };

  const onInputKeyDown = (event: KeyboardEvent<HTMLInputElement>): void => {
    if (event.key === 'Escape') {
      event.preventDefault();
      close();
    } else if (event.key === 'ArrowDown') {
      event.preventDefault();
      if (hits.length > 0) listRef.current?.focus();
    } else if (event.key === 'Enter') {
      event.preventDefault();
      openHit(hits[cursor]);
    }
  };

  const onListKeyDown = (event: KeyboardEvent<HTMLUListElement>): void => {
    switch (event.key) {
      case 'j':
      case 'n':
      case 'ArrowDown':
        event.preventDefault();
        move(1);
        break;
      case 'k':
      case 'N':
      case 'ArrowUp':
        event.preventDefault();
        move(-1);
        break;
      case 'Enter':
        event.preventDefault();
        openHit(hits[cursor]);
        break;
      case 'Escape':
        event.preventDefault();
        close();
        break;
      case '/':
        event.preventDefault();
        inputRef.current?.focus();
        break;
      default:
        break;
    }
  };

  return (
    <div className="nt-sidebar__search">
      <button
        type="button"
        className="nt-sidebar__act"
        onClick={() => setOpen((value) => !value)}
        aria-expanded={open}
        aria-label="搜索全部域"
        title="搜索（工具 / 技能 / 智能体 / 会话 / 记忆）"
        data-testid="workspace-search-trigger"
      >
        <span aria-hidden>🔍</span>
        {collapsed ? null : <span className="nt-sidebar__label">搜索</span>}
      </button>
      {open && (
        <div className="nt-sidebar__search-panel" data-testid="workspace-search-panel">
          <input
            ref={inputRef}
            type="search"
            className="nt-sidebar__search-input"
            placeholder="搜索全部域…"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            onKeyDown={onInputKeyDown}
            data-testid="search-input"
            aria-label="跨域搜索"
          />
          {error ? (
            <p className="nt-sidebar__search-error" role="alert">
              {error}
            </p>
          ) : null}
          {searching && hits.length === 0 ? (
            <p className="nt-sidebar__search-hint">搜索中…</p>
          ) : null}
          {!searching && trimmed && hits.length === 0 && !error ? (
            <p className="nt-sidebar__search-hint">没有命中。</p>
          ) : null}
          {hits.length > 0 && (
            <ul
              ref={listRef}
              className="nt-sidebar__search-results"
              tabIndex={0}
              onKeyDown={onListKeyDown}
              data-testid="search-results"
              aria-label="搜索结果"
            >
              {hits.map((hit, index) => {
                const path = pathForHit(hit);
                return (
                  <li
                    key={`${hit.domain}:${hit.id}`}
                    className={`nt-sidebar__search-hit${
                      index === cursor ? ' nt-sidebar__search-hit--cursor' : ''
                    }`}
                    data-cursor={index === cursor}
                    data-testid={`search-hit-${hit.domain}-${hit.id}`}
                    onClick={() => openHit(hit)}
                    onMouseEnter={() => setCursor(index)}
                  >
                    <span className="nt-sidebar__search-domain">
                      {DOMAIN_LABEL[hit.domain] ?? hit.domain}
                    </span>
                    <span className="nt-sidebar__search-title">{hit.title || hit.id}</span>
                    {hit.preview ? (
                      <span className="nt-sidebar__search-preview">{hit.preview}</span>
                    ) : null}
                    {path ? null : (
                      <span className="nt-sidebar__search-hint">（无页面，仅预览）</span>
                    )}
                  </li>
                );
              })}
            </ul>
          )}
          <p className="nt-sidebar__search-footer">↑↓ 选择 · Enter 打开 · Esc 关闭</p>
        </div>
      )}
    </div>
  );
}
