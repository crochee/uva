/**
 * 自动化 — schedules, execution history, task templates.
 *
 * The three tabs are the prototype's: 已配置 (schedules),
 * 执行历史 (delegations + eval suites), 任务模板 (empty until a template
 * layout exists). `手动新建` creates a real agent-spawning schedule
 * through `schedules/create`; `⏱ 在对话中创建` hands the job to the
 * conversation, where an agent can build it.
 *
 * Everything is read from the live services — no sample rows. A list
 * that fails reports itself in its own tab instead of blanking the
 * page.
 */
import { useEffect, useState, type FormEvent } from 'react';
import { useNavigate } from 'react-router-dom';
import { agentsApi } from '../../api/agents';
import { evalsApi } from '../../api/evals';
import { schedulesApi } from '../../api/schedules';
import { tasksApi } from '../../api/tasks';
import type {
  CreateScheduleRequest,
  EvalSuite,
  JobKind,
  ScheduleSummary,
  TaskDelegation,
} from '../../api/types';
import { ScheduleCard } from '../../components/schedules/ScheduleCard';
import { Button } from '../../components/ui/Button';
import { Input } from '../../components/ui/Input';
import { Modal } from '../../components/ui/Modal';
import { useToast } from '../../hooks/useToast';
import { shortId } from '../../lib/short-id';
import { RowCard } from '../RowCard';
import { SettingsCard } from '../SettingsCard';
import type { SettingsSectionProps } from '../sections';

type Tab = 'configured' | 'history' | 'templates';

const TABS: ReadonlyArray<{ id: Tab; label: string }> = [
  { id: 'configured', label: '已配置' },
  { id: 'history', label: '执行历史' },
  { id: 'templates', label: '任务模板' },
];

type ScheduleFormKind = 'interval' | 'cron' | 'once';

interface CreateForm {
  name: string;
  prompt: string;
  agent: string;
  kind: ScheduleFormKind;
  intervalSecs: string;
  cronExpr: string;
  timeoutSecs: string;
}

const EMPTY_FORM: CreateForm = {
  name: '',
  prompt: '',
  agent: '',
  kind: 'interval',
  intervalSecs: '3600',
  cronExpr: '0 * * * *',
  timeoutSecs: '300',
};

function toJobKind(form: CreateForm): JobKind {
  if (form.kind === 'interval') {
    return {
      kind: 'interval',
      interval: { secs: Math.max(1, Number(form.intervalSecs) || 1), nanos: 0 },
    };
  }
  if (form.kind === 'cron') return { kind: 'cron', expr: form.cronExpr.trim() };
  return { kind: 'once' };
}

export function AutomationSection({ reloadToken }: SettingsSectionProps) {
  const navigate = useNavigate();
  const toast = useToast();
  const [tab, setTab] = useState<Tab>('configured');
  const [schedules, setSchedules] = useState<ScheduleSummary[] | null>(null);
  const [tasks, setTasks] = useState<TaskDelegation[] | null>(null);
  const [evals, setEvals] = useState<EvalSuite[] | null>(null);
  const [errors, setErrors] = useState<string[]>([]);
  const [version, setVersion] = useState(0);

  const [formOpen, setFormOpen] = useState(false);
  const [form, setForm] = useState<CreateForm>(EMPTY_FORM);
  const [creating, setCreating] = useState(false);
  const [defaultAgent, setDefaultAgent] = useState<string>('');

  useEffect(() => {
    const controller = new AbortController();
    const settle = Promise.allSettled([
      schedulesApi.list(controller.signal),
      tasksApi.list({ limit: 50 }, controller.signal),
      evalsApi.list(controller.signal),
      agentsApi.defaultAgent(controller.signal),
    ]);
    void settle.then(([scheduleResult, taskResult, evalResult, agentResult]) => {
      const problems: string[] = [];
      if (scheduleResult.status === 'fulfilled') setSchedules(scheduleResult.value.data);
      else if (scheduleResult.reason.name !== 'AbortError') {
        problems.push(`schedules: ${scheduleResult.reason.message}`);
      }
      if (taskResult.status === 'fulfilled') {
        setTasks(taskResult.value.data);
      } else if (taskResult.reason.name !== 'AbortError') {
        problems.push(`tasks: ${taskResult.reason.message}`);
      }
      if (evalResult.status === 'fulfilled') setEvals(evalResult.value.data);
      else if (evalResult.reason.name !== 'AbortError') {
        problems.push(`evals: ${evalResult.reason.message}`);
      }
      if (agentResult.status === 'fulfilled') setDefaultAgent(agentResult.value.name);
      setErrors(problems);
    });
    return () => controller.abort();
  }, [reloadToken, version]);

  const submit = async (event: FormEvent<HTMLFormElement>): Promise<void> => {
    event.preventDefault();
    const request: CreateScheduleRequest = {
      name: form.name.trim(),
      kind: toJobKind(form),
      payload: {
        kind: 'agent',
        agent_ref: form.agent.trim() || defaultAgent || 'default',
        prompt_template: form.prompt,
        timeout: Math.max(1, Number(form.timeoutSecs) || 60),
      },
    };
    setCreating(true);
    try {
      const created = await schedulesApi.create(request);
      toast.push({ variant: 'success', message: `已创建计划 "${created.name}"。` });
      setFormOpen(false);
      setForm(EMPTY_FORM);
      setVersion((value) => value + 1);
    } catch (cause) {
      toast.push({
        variant: 'error',
        message: `创建失败：${cause instanceof Error ? cause.message : String(cause)}`,
        durationMs: 6000,
      });
    } finally {
      setCreating(false);
    }
  };

  const describeErrors = (prefix: string) =>
    errors
      .filter((problem) => problem.startsWith(prefix))
      .map((problem) => (
        <p key={problem} className="nt-settings__error" role="alert">
          {problem}
        </p>
      ));

  return (
    <>
      <SettingsCard
        title="自动化"
        description="配置和管理自动化任务，让 uva 按计划执行。"
        actions={
          <>
            <Button
              size="1"
              variant="soft"
              onClick={() => setFormOpen(true)}
              data-testid="automation-create"
            >
              手动新建
            </Button>
            <Button
              size="1"
              variant="solid"
              onClick={() => navigate('/chat')}
              data-testid="automation-create-in-chat"
            >
              ⏱ 在对话中创建
            </Button>
          </>
        }
        testId="automation-card"
      >
        <div className="nt-settings__tabs" role="tablist" aria-label="自动化视图">
          {TABS.map((entry) => (
            <button
              key={entry.id}
              type="button"
              role="tab"
              aria-selected={tab === entry.id}
              className={`nt-settings__tab${tab === entry.id ? ' nt-settings__tab--on' : ''}`}
              onClick={() => setTab(entry.id)}
              data-testid={`automation-tab-${entry.id}`}
            >
              {entry.label}
            </button>
          ))}
        </div>

        {tab === 'configured' && (
          <div data-testid="automation-configured">
            {describeErrors('schedules')}
            <h3 className="nt-settings__group-title">计划</h3>
            {!schedules ? (
              <p className="nt-settings__empty">正在读取计划…</p>
            ) : schedules.length === 0 ? (
              <p className="nt-settings__empty">还没有计划。</p>
            ) : (
              schedules.map((schedule) => <ScheduleCard key={schedule.id} schedule={schedule} />)
            )}
          </div>
        )}

        {tab === 'history' && (
          <div data-testid="automation-history">
            {describeErrors('tasks')}
            <h3 className="nt-settings__group-title">委派</h3>
            {!tasks ? (
              <p className="nt-settings__empty">正在读取执行历史…</p>
            ) : tasks.length === 0 ? (
              <p className="nt-settings__empty">还没有执行记录。</p>
            ) : (
              <ul className="nt-settings__history">
                {tasks.map((task) => (
                  <li key={task.id} className="nt-settings__history-row">
                    <span className="nt-settings__history-agent">{task.agent}</span>
                    <span>{task.prompt.slice(0, 80)}</span>
                    <span className="nt-settings__history-status">{task.status}</span>
                    {task.child_session_id ? (
                      <button
                        type="button"
                        className="nt-settings__link"
                        onClick={() => navigate(`/chat/${task.child_session_id}`)}
                        data-testid={`task-open-${task.id}`}
                      >
                        {shortId(task.child_session_id)}
                      </button>
                    ) : null}
                  </li>
                ))}
              </ul>
            )}
            {describeErrors('evals')}
            <h3 className="nt-settings__group-title">评测</h3>
            {!evals ? (
              <p className="nt-settings__empty">正在读取评测套件…</p>
            ) : evals.length === 0 ? (
              <p className="nt-settings__empty">还没有评测套件。</p>
            ) : (
              <ul className="nt-settings__rows">
                {evals.map((suite) => (
                  <RowCard
                    key={suite.name}
                    icon="📊"
                    label={suite.name}
                    description={`${suite.cases.length} 个用例`}
                    testId={`eval-row-${suite.name}`}
                  />
                ))}
              </ul>
            )}
          </div>
        )}

        {tab === 'templates' && (
          <div data-testid="automation-templates">
            <p className="nt-settings__empty">
              尚未配置自动化任务。模板来自 plugin 排布，当前部署没有提供模板面。
            </p>
            <button
              type="button"
              className="nt-settings__reload"
              disabled
              title="模板面来自 plugin 排布；当前部署没有提供"
              data-testid="automation-from-template"
            >
              从模板创建 →
            </button>
          </div>
        )}
      </SettingsCard>

      <Modal
        open={formOpen}
        onClose={() => setFormOpen(false)}
        title="手动新建自动化"
        titleId="nt-automation-create-title"
        testId="automation-create-modal"
        footer={
          <>
            <Button variant="ghost" color="gray" onClick={() => setFormOpen(false)}>
              取消
            </Button>
            <Button type="submit" form="nt-automation-form" loading={creating}>
              创建
            </Button>
          </>
        }
      >
        <form id="nt-automation-form" onSubmit={(event) => void submit(event)}>
          <Input
            label="名称"
            value={form.name}
            onChange={(event) => setForm({ ...form, name: event.target.value })}
            placeholder="daily-report"
            data-testid="automation-name"
          />
          <Input
            label="提示词模板"
            value={form.prompt}
            onChange={(event) => setForm({ ...form, prompt: event.target.value })}
            placeholder="总结今天的变更并写入报告"
            data-testid="automation-prompt"
          />
          <Input
            label="Agent"
            value={form.agent}
            onChange={(event) => setForm({ ...form, agent: event.target.value })}
            placeholder={defaultAgent || '默认 agent'}
            data-testid="automation-agent"
          />
          <label className="nt-settings__control-row" htmlFor="automation-kind">
            <span>计划类型</span>
            <select
              id="automation-kind"
              className="nt-settings__select"
              value={form.kind}
              onChange={(event) =>
                setForm({ ...form, kind: event.target.value as ScheduleFormKind })
              }
              data-testid="automation-kind"
            >
              <option value="interval">固定间隔</option>
              <option value="cron">cron</option>
              <option value="once">一次性</option>
            </select>
          </label>
          {form.kind === 'interval' && (
            <Input
              label="间隔（秒）"
              value={form.intervalSecs}
              onChange={(event) => setForm({ ...form, intervalSecs: event.target.value })}
              data-testid="automation-interval"
            />
          )}
          {form.kind === 'cron' && (
            <Input
              label="cron 表达式"
              value={form.cronExpr}
              onChange={(event) => setForm({ ...form, cronExpr: event.target.value })}
              data-testid="automation-cron"
            />
          )}
          <Input
            label="超时（秒）"
            value={form.timeoutSecs}
            onChange={(event) => setForm({ ...form, timeoutSecs: event.target.value })}
            data-testid="automation-timeout"
          />
        </form>
      </Modal>
    </>
  );
}
