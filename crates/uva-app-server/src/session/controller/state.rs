//! 控制器状态的转移表：一个模块拥有「怎么走」。
//!
//! - 表是一个封闭转移枚举 + 一个穷尽 [`apply`]（全函数）：每个
//!   （当前状态 × 转移）组合都有定义的结果——迁移或**有意的停留**；
//!   无错误通道、无 panic。no-op 是承重的格：取消后排干的 settle
//!   不得覆盖 `Cancelled`，接管不得覆盖活运行。
//! - 快照发布归这张表，映射是**部分的**：只有 `OperationState` 有
//!   对应物的转移才发帧——`OperationState` 是一次运行的生命周期，
//!   没有「等待审批」这一格（ADR-0008 的两套词汇裁定）。
//! - [`Transition::Settle`] 的帧在 **flush 之后**发（
//!   [`ControllerState::settle_flushed`]）：订阅者先看到行落盘、
//!   再看到运行收尾，翻转与发帧因此是两次调用。
//! - `suspended` 是本模块的私有记录：Suspended 事件记账
//!   （[`ControllerState::note_suspension`]）、settle 消费、
//!   [`Transition::BeginRun`] 清零。延迟翻转是有意的——立即翻转会让
//!   `is_startable` 在旧任务排干时放行新 run。
//!
//! 读半边（会话状态的纯折叠）住在 [`crate::session::status`]；
//! `SessionState` 枚举是读写两半共享的词汇，定义在那里。

use std::sync::{
    Arc,
    Mutex,
    atomic::{AtomicBool, Ordering},
};

use uva::session::{OperationSnapshot, OperationState, SnapshotBus};

use crate::session::status::SessionState;

/// 终态帧沿历史形状带固定的 agent 名：一次运行的真实 agent 名只在
/// `Running` 帧上（[`Transition::BeginRun`] 带它），收尾帧不重读
/// 依赖。
const TERMINAL_FRAME_AGENT: &str = "default";

/// 转移表的一行：控制器状态的一次「怎么走」。
///
/// 载荷字段是发帧要用的**事实**，不是调用方替模块做的决定。
#[derive(Debug)]
pub(super) enum Transition {
    /// 开跑：无条件迁移到 `Running`。前置条件由调用方先问
    /// [`ControllerState::is_startable`]——op 循环串行化保证了
    /// 「问过再走」之间没有人插队。
    BeginRun {
        /// 运行真正使用的 agent 名（`Running` 帧的载荷）。
        agent_name: String,
        /// 生效的迭代上限（`Running` 帧的载荷）。
        max_iterations: usize,
    },
    /// 一次运行收尾：只从 `Running` 迁移；`suspended` 决定落在
    /// 等待审批还是空闲。
    Settle {
        /// 收尾时日志里有没有未决审批（模块私有的
        /// `suspended` 记录被 [`ControllerState::settle`] 消费）。
        suspended: bool,
    },
    /// 取消：无条件迁移到 `Cancelled`——取消是对开着审批的作答，
    /// 先于排干中的 settle 落地。
    Cancelled {
        /// 取消原因（`Cancelled` 帧的载荷）。
        reason: String,
        /// 生效的迭代上限（`Cancelled` 帧的载荷）。
        max_iterations: usize,
    },
    /// 日志说有待审批而 op 拒绝开跑：从 Idle / Cancelled 记下标记。
    ApprovalAwaited,
    /// 审批已答、挂起的轮将重跑：`AwaitingApproval` → `Idle`。
    ResumedParked,
    /// 接管：从日志权威重建。拒绝覆盖活运行——接管发生在本控制器
    /// 开跑之前，一个 `Running` 只属于正在跑它的那个节点。
    Adopt {
        /// 从会话日志读出的重建值。
        state: SessionState,
    },
}

/// 全函数转移表：每个组合都有定义的结果，停留也是一格。
pub(super) fn apply(
    current: SessionState,
    transition: &Transition,
) -> SessionState {
    use SessionState::{AwaitingApproval, Cancelled, Idle, Running};
    match transition {
        Transition::BeginRun { .. } => Running,
        Transition::Settle { suspended } => match current {
            Running => {
                if *suspended {
                    AwaitingApproval
                } else {
                    Idle
                }
            }
            // 取消已裁决：排干中的 settle 不得覆盖 `Cancelled`。
            settled => settled,
        },
        Transition::Cancelled { .. } => Cancelled,
        Transition::ApprovalAwaited => match current {
            Idle | Cancelled => AwaitingApproval,
            // 不覆盖活运行，也不重复标记。
            marked => marked,
        },
        Transition::ResumedParked => match current {
            AwaitingApproval => Idle,
            other => other,
        },
        Transition::Adopt { state } => match current {
            Running => Running,
            _ => *state,
        },
    }
}

/// 控制器状态的 owner：转移表、写路径与快照发布住在这一处。
///
/// 句柄（[`super::SessionController`]）与
/// [`super::inner::ControllerInner`] 共享同一个 `Arc`，与被替换的
/// `Arc<Mutex<SessionState>>` 同一个共享形状。
pub(super) struct ControllerState {
    session_id: String,
    state: Mutex<SessionState>,
    /// Suspended 事件到 settle 决策之间的单比特运输（模块私有）。
    suspended: AtomicBool,
    snapshot_bus: SnapshotBus,
}

impl ControllerState {
    /// 造一个空闲的控制器状态；`snapshot_bus` 与句柄共享同一总线。
    pub(super) fn new(
        session_id: String,
        snapshot_bus: SnapshotBus,
    ) -> Arc<Self> {
        Arc::new(Self {
            session_id,
            state: Mutex::new(SessionState::Idle),
            suspended: AtomicBool::new(false),
            snapshot_bus,
        })
    }

    /// 走一次转移：翻转状态，`BeginRun` / `Cancelled` 的快照帧
    /// 随转移立即发布；`Settle` 的帧留给
    /// [`Self::settle_flushed`]（行先落盘）。
    pub(super) fn advance(&self, transition: Transition) -> SessionState {
        let next = {
            let mut state = self.state.lock().expect("state mutex poisoned");
            let next = apply(*state, &transition);
            *state = next;
            next
        };
        match &transition {
            Transition::BeginRun {
                agent_name,
                max_iterations,
            } => {
                // 新 run 不继承上一轮的挂起记录。
                self.suspended.store(false, Ordering::SeqCst);
                let _seq =
                    self.snapshot_bus.publish(OperationSnapshot::started(
                        self.session_id.clone(),
                        agent_name.clone(),
                        *max_iterations,
                    ));
            }
            Transition::Cancelled {
                reason,
                max_iterations,
            } => {
                let _seq = self.snapshot_bus.publish(OperationSnapshot::new(
                    self.session_id.clone(),
                    TERMINAL_FRAME_AGENT.to_string(),
                    OperationState::Cancelled {
                        reason: reason.clone(),
                    },
                    0,
                    *max_iterations,
                    uva::provider::TokenUsage::default(),
                ));
            }
            _ => {}
        }
        next
    }

    /// settle 的翻转半：消费私有的 `suspended` 记录并走
    /// [`Transition::Settle`]。帧在 flush 之后由
    /// [`Self::settle_flushed`] 发。
    pub(super) fn settle(&self) -> SessionState {
        let suspended = self.suspended.swap(false, Ordering::SeqCst);
        self.advance(Transition::Settle { suspended })
    }

    /// settle 的发布半：run task 把行 flush 落盘之后调用——订阅者
    /// 先看到 durable 行，再看到运行收尾。两帧：`Completing` 是
    /// 最后一轮的**结局**（流上的消费者仍能读到它怎么结束），
    /// `Idle` 是会话的**当下**——`latest` 从此报「现在」，而不是
    /// 最后一轮的遗迹（这正是 `delete_session` 判断忙碌的依据）。
    pub(super) fn settle_flushed(&self, max_iterations: usize) {
        let frame = |state: OperationState| {
            OperationSnapshot::new(
                self.session_id.clone(),
                TERMINAL_FRAME_AGENT.to_string(),
                state,
                0,
                max_iterations,
                uva::provider::TokenUsage::default(),
            )
        };
        let _seq = self.snapshot_bus.publish(frame(OperationState::Completing));
        let _seq = self.snapshot_bus.publish(frame(OperationState::Idle));
    }

    /// Suspended 事件记账：这一轮 run 收尾时落在等待审批。
    pub(super) fn note_suspension(&self) {
        self.suspended.store(true, Ordering::SeqCst);
    }

    /// 当前的控制器状态。
    pub(super) fn current(&self) -> SessionState {
        *self.state.lock().expect("state mutex poisoned")
    }

    /// 有活着的 run 吗（steer 与 resume 拒绝的判据）。
    pub(super) fn is_running(&self) -> bool {
        self.current() == SessionState::Running
    }

    /// [`Transition::BeginRun`] 的前置条件：只有 Idle / Cancelled
    /// 可以开跑（Cancelled 可以，因为取消标记状态后
    /// `maybe_start_run` 会再唤醒控制器——见 Cancel op 处理）。
    pub(super) fn is_startable(&self) -> bool {
        let state = self.current();
        if !matches!(state, SessionState::Idle | SessionState::Cancelled) {
            tracing::trace!(
                target: "uva.session",
                session_id = %self.session_id,
                state = ?state,
                "maybe_start_run: skipped (not Idle/Cancelled)"
            );
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use SessionState::{AwaitingApproval, Cancelled, Idle, Running};

    use super::*;

    fn begin_run() -> Transition {
        Transition::BeginRun {
            agent_name: "default".to_string(),
            max_iterations: 25,
        }
    }

    fn cancel() -> Transition {
        Transition::Cancelled {
            reason: "cancelled".to_string(),
            max_iterations: 25,
        }
    }

    fn settle(suspended: bool) -> Transition {
        Transition::Settle { suspended }
    }

    /// The whole table, pinned cell by cell: every (current ×
    /// transition) pair has a defined outcome, and a stay is a cell
    /// — not an accident.
    #[test]
    fn transition_table_is_total_and_pinned() {
        #[derive(Debug)]
        struct Case(SessionState, Transition, SessionState);
        let cases = [
            // BeginRun: unconditional — the op loop serializes the
            // is_startable probe and the walk.
            Case(Idle, begin_run(), Running),
            Case(Running, begin_run(), Running),
            Case(AwaitingApproval, begin_run(), Running),
            Case(Cancelled, begin_run(), Running),
            // Settle: only from Running; the suspension record picks
            // the landing spot.
            Case(Idle, settle(true), Idle),
            Case(Idle, settle(false), Idle),
            Case(Running, settle(true), AwaitingApproval),
            Case(Running, settle(false), Idle),
            Case(AwaitingApproval, settle(true), AwaitingApproval),
            Case(AwaitingApproval, settle(false), AwaitingApproval),
            Case(Cancelled, settle(true), Cancelled),
            Case(Cancelled, settle(false), Cancelled),
            // Cancelled: unconditional — cancelling answers the open
            // asks, and the drain-time settle must not overwrite it.
            Case(Idle, cancel(), Cancelled),
            Case(Running, cancel(), Cancelled),
            Case(AwaitingApproval, cancel(), Cancelled),
            Case(Cancelled, cancel(), Cancelled),
            // ApprovalAwaited: records the log-derived pause from the
            // two settled states; never over a live run.
            Case(Idle, Transition::ApprovalAwaited, AwaitingApproval),
            Case(Running, Transition::ApprovalAwaited, Running),
            Case(
                AwaitingApproval,
                Transition::ApprovalAwaited,
                AwaitingApproval,
            ),
            Case(Cancelled, Transition::ApprovalAwaited, AwaitingApproval),
            // ResumedParked: leaves the paused state only.
            Case(Idle, Transition::ResumedParked, Idle),
            Case(Running, Transition::ResumedParked, Running),
            Case(AwaitingApproval, Transition::ResumedParked, Idle),
            Case(Cancelled, Transition::ResumedParked, Cancelled),
            // Adopt: the log's word wins — except over a live run.
            Case(Idle, Transition::Adopt { state: Cancelled }, Cancelled),
            Case(Running, Transition::Adopt { state: Idle }, Running),
            Case(AwaitingApproval, Transition::Adopt { state: Idle }, Idle),
            Case(
                Cancelled,
                Transition::Adopt {
                    state: AwaitingApproval,
                },
                AwaitingApproval,
            ),
        ];
        for Case(current, transition, expected) in &cases {
            assert_eq!(
                apply(*current, transition),
                *expected,
                "{current:?} × {transition:?}",
            );
        }
    }

    fn subject() -> Arc<ControllerState> {
        ControllerState::new("s1".to_string(), SnapshotBus::new())
    }

    /// The settle transition consumes the module's private
    /// suspension record; a second settle has nothing to consume
    /// and stays put.
    #[test]
    fn settle_consumes_the_suspension_record_once() {
        let state = subject();
        state.advance(begin_run());
        state.note_suspension();
        assert_eq!(state.settle(), AwaitingApproval);
        assert_eq!(state.settle(), AwaitingApproval);
    }

    /// A fresh run does not inherit the previous turn's suspension.
    #[test]
    fn begin_run_clears_the_suspension_record() {
        let state = subject();
        state.note_suspension();
        state.advance(begin_run());
        assert_eq!(state.settle(), Idle);
    }

    /// The run's frames in order: Running at the start, then
    /// Completing (the outcome) and Idle (the now) after the flush —
    /// `latest` reports the present, not the last run's residue.
    #[tokio::test]
    async fn the_bus_sees_running_then_completing_then_idle() {
        let state = subject();
        let (mut rx, latest) = state.snapshot_bus.subscribe();
        assert!(latest.is_none());
        state.advance(begin_run());
        state.settle();
        state.settle_flushed(25);
        let running = rx.next().await.expect("Running frame");
        assert!(matches!(
            running.state,
            OperationState::Running { iteration: 0 }
        ));
        let completing = rx.next().await.expect("Completing frame");
        assert_eq!(completing.state, OperationState::Completing);
        let idle = rx.next().await.expect("Idle frame");
        assert_eq!(idle.state, OperationState::Idle);
        let (_, latest) = state.snapshot_bus.subscribe();
        assert_eq!(
            latest.expect("a settled bus holds a frame").state,
            OperationState::Idle
        );
    }

    /// Cancelling mid-run decides the outcome: the drain-time settle
    /// must not resurrect anything, and a cancelled session is still
    /// startable (the cancel handler wakes the controller).
    #[test]
    fn cancel_decides_and_settle_does_not_overwrite() {
        let state = subject();
        state.advance(begin_run());
        state.note_suspension();
        state.advance(cancel());
        assert_eq!(state.settle(), Cancelled);
        assert!(state.is_startable());
    }
}
