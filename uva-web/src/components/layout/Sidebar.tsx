/**
 * The sidebar — two modes on one rail.
 *
 * - **workspace** (every route except `/settings`): brand → 新会话 →
 *   工作区 tree → user entry. This is the conversation-first shell
 *   (design §6.1): the capability catalogues moved into settings, so
 *   the sidebar's job is the workspace and the sessions in it.
 * - **settings** (`/settings/*`): the whole rail becomes the settings
 *   navigation — user card → filter box → section list.
 *
 * Both modes keep every label in the DOM when collapsed (design review
 * focus 4): the glyph stays visible, the label becomes visually hidden
 * but readable, and `title` supplies the sighted tooltip. A screen
 * reader never loses the destination.
 *
 * The tree's root is the constant "默认工作区" (D35): this server has
 * one workspace and no workspace RPC, so a fake selector would be a
 * lie. The two extra header actions are rendered disabled with that
 * reason.
 */
import { useCallback, useEffect, useRef, useState } from 'react';
import { NavLink, useLocation, useNavigate } from 'react-router-dom';
import { Box, Flex } from '@radix-ui/themes';
import { sessionsApi } from '../../api/sessions';
import { SETTINGS_NAV_SECTIONS } from '../../settings/sections';
import { useListNavigation } from '../../hooks/useListNavigation';
import { useToast } from '../../hooks/useToast';
import { useWorkspaceTree } from '../../hooks/useWorkspaceTree';
import { shortId } from '../../lib/short-id';
import { Button } from '../ui/Button';
import { Modal } from '../ui/Modal';
import { WorkspaceSearch } from './WorkspaceSearch';
import './Sidebar.css';

export interface SidebarProps {
  /** Render the icon rail instead of the labelled column. */
  collapsed: boolean;
  /** Omitted when a control could not change anything (narrow
   *  viewport, where the rail is forced) — the toggle disappears
   *  rather than sitting there inert. */
  onToggle?: () => void;
}

export function Sidebar({ collapsed, onToggle }: SidebarProps) {
  const { pathname } = useLocation();
  const settingsMode = pathname.startsWith('/settings');
  return (
    <Box asChild className={`nt-sidebar${collapsed ? ' nt-sidebar--collapsed' : ''}`}>
      <nav aria-label={settingsMode ? '设置导航' : '主导航'} id="nt-sidebar-nav">
        {settingsMode ? (
          <SettingsNav />
        ) : (
          <WorkspaceNav collapsed={collapsed} onToggle={onToggle} />
        )}
      </nav>
    </Box>
  );
}

function CollapseButton({ collapsed, onToggle }: { collapsed: boolean; onToggle?: () => void }) {
  if (!onToggle) return null;
  return (
    <button
      type="button"
      className="nt-sidebar__toggle"
      onClick={onToggle}
      aria-expanded={!collapsed}
      aria-controls="nt-sidebar-nav"
      aria-label={collapsed ? 'Expand sidebar' : 'Collapse sidebar'}
      title={`${collapsed ? 'Expand' : 'Collapse'} sidebar (Ctrl+B)`}
      data-testid="sidebar-toggle"
    >
      <span aria-hidden>{collapsed ? '»' : '«'}</span>
    </button>
  );
}

function WorkspaceNav({ collapsed, onToggle }: { collapsed: boolean; onToggle?: () => void }) {
  const tree = useWorkspaceTree();
  const navigate = useNavigate();
  const toast = useToast();
  const [treeOpen, setTreeOpen] = useState(true);
  const treeRef = useRef<HTMLDivElement | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<string | null>(null);
  const [deleting, setDeleting] = useState(false);

  const openSession = useCallback(
    (id: string) => navigate(`/chat/${encodeURIComponent(id)}`),
    [navigate],
  );
  const requestDelete = useCallback((id: string) => setDeleteTarget(id), []);
  const copyId = useCallback(
    (id: string) => {
      navigator.clipboard
        .writeText(id)
        .then(() => toast.push({ variant: 'success', message: `已复制 ${shortId(id)}` }))
        .catch(() => {
          // Clipboard denied (insecure context): the id stays visible
          // in the node's title, so the user can still select it.
        });
    },
    [toast],
  );

  // Vim cursor over the tree (design D30). `onOpen` doubles as the
  // `Enter` action; `x` asks for confirmation, `y` copies the full id.
  const nav = useListNavigation({
    itemIds: tree.sessions.map((session) => session.id),
    onOpen: openSession,
    onDelete: requestDelete,
    onCopyId: copyId,
    containerRef: treeRef,
  });

  const confirmDelete = async (): Promise<void> => {
    if (deleteTarget === null) return;
    setDeleting(true);
    try {
      await sessionsApi.remove(deleteTarget);
      toast.push({ variant: 'success', message: `Deleted session "${shortId(deleteTarget)}".` });
      await tree.refresh();
    } catch (cause) {
      toast.push({
        variant: 'error',
        message: cause instanceof Error ? cause.message : String(cause),
      });
    } finally {
      setDeleting(false);
      setDeleteTarget(null);
    }
  };

  return (
    <Flex direction="column" gap="1" p="3" className="nt-sidebar__body">
      <Flex
        align="center"
        justify="between"
        className="nt-sidebar__head"
        data-testid="sidebar-brand"
      >
        <Flex align="center" gap="2" className="nt-sidebar__brand">
          <span aria-hidden className="nt-sidebar__logo">
            ◐
          </span>
          <span className="nt-sidebar__label nt-sidebar__brand-name">uva</span>
          <span className="nt-sidebar__badge">HARNESS</span>
        </Flex>
        <CollapseButton collapsed={collapsed} onToggle={onToggle} />
      </Flex>

      <NavLink
        to="/chat"
        className="nt-sidebar__new"
        title={collapsed ? '新会话' : undefined}
        data-testid="new-session"
      >
        <span aria-hidden>🕐</span>
        <span className="nt-sidebar__label">新会话</span>
      </NavLink>

      <div className="nt-sidebar__group-head">
        <span className="nt-sidebar__label nt-sidebar__group-title">工作区</span>
        <span className="nt-sidebar__acts">
          <WorkspaceSearch collapsed={collapsed} />
          <button
            type="button"
            className="nt-sidebar__act"
            disabled
            title="多工作区面随后续刀次提供"
            aria-label="切换工作区（随后续刀次提供）"
          >
            <span aria-hidden>⇅</span>
          </button>
          <button
            type="button"
            className="nt-sidebar__act"
            disabled
            title="多工作区面随后续刀次提供"
            aria-label="工作区目录（随后续刀次提供）"
          >
            <span aria-hidden>📁</span>
          </button>
        </span>
      </div>

      <div className="nt-sidebar__tree" data-testid="workspace-tree" ref={treeRef}>
        <button
          type="button"
          className="nt-sidebar__node nt-sidebar__node--root"
          onClick={() => setTreeOpen((value) => !value)}
          aria-expanded={treeOpen}
          title={collapsed ? '默认工作区' : undefined}
          data-testid="workspace-root"
          data-node-id="workspace-root"
        >
          <span aria-hidden className="nt-sidebar__caret">
            {treeOpen ? '▾' : '▸'}
          </span>
          <span aria-hidden className="nt-sidebar__node-icon">
            🗂
          </span>
          <span className="nt-sidebar__label">默认工作区</span>
        </button>
        {treeOpen &&
          tree.sessions.map((session) => (
            <NavLink
              key={session.id}
              to={`/chat/${encodeURIComponent(session.id)}`}
              className={({ isActive }) =>
                `nt-sidebar__node nt-sidebar__node--child${
                  isActive ? ' nt-sidebar__node--active' : ''
                }${session.id === nav.cursor ? ' nt-sidebar__node--cursor' : ''}`
              }
              title={shortId(session.id)}
              data-testid={`session-node-${session.id}`}
              data-node-id={session.id}
              data-cursor={session.id === nav.cursor}
            >
              <span className="nt-sidebar__label">{shortId(session.id)}</span>
              <span className="nt-sidebar__node-status">{session.status}</span>
            </NavLink>
          ))}
        {tree.loading && tree.sessions.length === 0 ? (
          <p className="nt-sidebar__hint">载入会话…</p>
        ) : null}
        {tree.error ? <p className="nt-sidebar__hint">{tree.error}</p> : null}
      </div>

      <div className="nt-sidebar__spacer" />

      <NavLink
        to="/settings/account"
        className="nt-sidebar__user"
        title="设置"
        data-testid="user-entry"
      >
        <span aria-hidden>🧑</span>
        <span className="nt-sidebar__label">dev</span>
      </NavLink>

      <Modal
        open={deleteTarget !== null}
        onClose={() => setDeleteTarget(null)}
        title="删除会话"
        titleId="nt-session-delete-title"
        testId="session-delete-modal"
        footer={
          <>
            <Button variant="ghost" color="gray" onClick={() => setDeleteTarget(null)}>
              取消
            </Button>
            <Button color="red" loading={deleting} onClick={() => void confirmDelete()}>
              删除
            </Button>
          </>
        }
      >
        <p>
          删除会话 <code>{deleteTarget === null ? '' : shortId(deleteTarget)}</code>{' '}
          及其持久化记录？此操作不可撤销。
        </p>
      </Modal>
    </Flex>
  );
}

function SettingsNav() {
  const [filter, setFilter] = useState('');
  const searchRef = useRef<HTMLInputElement | null>(null);
  const listRef = useRef<HTMLDivElement | null>(null);
  const navigate = useNavigate();

  // The prototype's `Ctrl+F 搜索`. Handled here (settings mode only) so
  // the browser's find dialog does not steal the cue the rail prints.
  // `/` dispatches the same focus from the global shortcut hook.
  useEffect(() => {
    function onKeyDown(event: KeyboardEvent): void {
      if (!event.metaKey && !event.ctrlKey) return;
      if (event.altKey || event.shiftKey) return;
      if (event.key.toLowerCase() !== 'f') return;
      event.preventDefault();
      searchRef.current?.focus();
      searchRef.current?.select();
    }
    function onFocusRequest(): void {
      searchRef.current?.focus();
      searchRef.current?.select();
    }
    window.addEventListener('keydown', onKeyDown);
    window.addEventListener('uva:settings-search', onFocusRequest);
    return () => {
      window.removeEventListener('keydown', onKeyDown);
      window.removeEventListener('uva:settings-search', onFocusRequest);
    };
  }, []);

  const needle = filter.trim().toLowerCase();
  const sections = SETTINGS_NAV_SECTIONS.filter(
    (section) =>
      needle === '' ||
      section.label.toLowerCase().includes(needle) ||
      section.id.includes(needle) ||
      section.description.toLowerCase().includes(needle),
  );

  // `j`/`k` over the section list; `Enter` opens. No delete/copy here —
  // a section is not an artifact.
  const nav = useListNavigation({
    itemIds: sections.map((section) => section.id),
    onOpen: (id) => navigate(`/settings/${id}`),
    containerRef: listRef,
  });

  return (
    <Flex direction="column" gap="1" p="3" className="nt-sidebar__body">
      <NavLink
        to="/settings/account"
        className={({ isActive }) =>
          `nt-sidebar__user-card${isActive ? ' nt-sidebar__user-card--active' : ''}`
        }
        data-testid="settings-user-card"
      >
        <span aria-hidden className="nt-sidebar__avatar">
          🧑
        </span>
        <span className="nt-sidebar__label nt-sidebar__user-name">dev</span>
        <span className="nt-sidebar__badge">免费</span>
      </NavLink>

      <input
        ref={searchRef}
        type="search"
        className="nt-sidebar__search-input"
        placeholder="Ctrl+F 搜索"
        value={filter}
        onChange={(event) => setFilter(event.target.value)}
        aria-label="过滤设置分区"
        data-testid="settings-search"
      />

      <div className="nt-sidebar__settings-list" data-testid="settings-section-list" ref={listRef}>
        {sections.map((section) => (
          <NavLink
            key={section.id}
            to={`/settings/${section.id}`}
            className={({ isActive }) =>
              `nt-sidebar__settings-item${isActive ? ' nt-sidebar__settings-item--active' : ''}${
                section.id === nav.cursor ? ' nt-sidebar__settings-item--cursor' : ''
              }`
            }
            title={section.label}
            data-testid={`settings-section-${section.id}`}
            data-node-id={section.id}
            data-cursor={section.id === nav.cursor}
          >
            <span aria-hidden className="nt-sidebar__node-icon">
              {section.icon}
            </span>
            <span className="nt-sidebar__label">{section.label}</span>
          </NavLink>
        ))}
        {sections.length === 0 ? <p className="nt-sidebar__hint">没有匹配的分区。</p> : null}
      </div>

      <div className="nt-sidebar__spacer" />

      <NavLink to="/chat" className="nt-sidebar__user" data-testid="settings-back-to-chat">
        <span aria-hidden>💬</span>
        <span className="nt-sidebar__label">返回对话</span>
      </NavLink>
    </Flex>
  );
}
