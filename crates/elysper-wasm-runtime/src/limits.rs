//! Fuel, linear-memory, and wall-clock ceilings for one call.
//!
//! The three limits are enforced at three different layers, because they
//! answer three different questions:
//!
//! - **fuel** bounds *work* — instructions executed, not time. It is the
//!   deterministic ceiling and the one a test can reproduce.
//! - **linear memory** bounds *state*. The store's [`ResourceLimiter`]
//!   refuses a growth past [`WasmLimits::max_memory_bytes`] and records
//!   the refusal, so a guest that traps after a refused growth maps to
//!   [`WasmErrorKind::MemoryLimit`](crate::WasmErrorKind::MemoryLimit)
//!   instead of a generic trap.
//! - **deadline** bounds *wall clock*. Wasmtime's epoch interruption is
//!   the only interrupt that reaches a guest stuck in an infinite loop;
//!   the runtime's ticker advances the engine epoch every
//!   [`EPOCH_TICK`], and each call sets a deadline measured in ticks.

use std::time::Duration;

use wasmtime::{Error, ResourceLimiter};

/// How often the runtime's epoch ticker advances the engine epoch.
///
/// A call's wall-clock deadline is this period rounded up: a 50 ms
/// deadline is 10 ticks. Smaller ticks give a tighter deadline and cost
/// one wake-up per tick per runtime; 5 ms bounds the overshoot a caller
/// can observe without turning an idle runtime into a busy thread.
pub const EPOCH_TICK: Duration = Duration::from_millis(5);

/// One call's resource budget.
///
/// The same budget arms every `describe()` and `execute()` invocation;
/// a fresh fuel allotment and epoch deadline are installed before each
/// call, so a long-lived pooled instance cannot bank unused fuel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WasmLimits {
    /// Instructions the guest may execute per call.
    pub fuel: u64,
    /// Maximum linear-memory size in bytes, across every memory the
    /// store owns.
    pub max_memory_bytes: usize,
    /// Wall-clock deadline per call. [`Duration::ZERO`] disables the
    /// interrupt; the call is then bounded by fuel and memory alone.
    pub deadline: Duration,
}

impl Default for WasmLimits {
    /// 10M instructions, 64 MiB of linear memory, 5 seconds.
    fn default() -> Self {
        Self {
            fuel: 10_000_000,
            max_memory_bytes: 64 * 1024 * 1024,
            deadline: Duration::from_secs(5),
        }
    }
}

impl WasmLimits {
    /// Build a budget from its three ceilings.
    #[must_use]
    pub const fn new(
        fuel: u64,
        max_memory_bytes: usize,
        deadline: Duration,
    ) -> Self {
        Self {
            fuel,
            max_memory_bytes,
            deadline,
        }
    }

    /// Replace the fuel budget.
    #[must_use]
    pub const fn with_fuel(mut self, fuel: u64) -> Self {
        self.fuel = fuel;
        self
    }

    /// Replace the linear-memory ceiling.
    #[must_use]
    pub const fn with_max_memory_bytes(mut self, bytes: usize) -> Self {
        self.max_memory_bytes = bytes;
        self
    }

    /// Replace the wall-clock deadline.
    #[must_use]
    pub const fn with_deadline(mut self, deadline: Duration) -> Self {
        self.deadline = deadline;
        self
    }

    /// Whether a wall-clock deadline is configured.
    #[must_use]
    pub const fn deadline_enabled(&self) -> bool {
        !self.deadline.is_zero()
    }

    /// The number of epoch ticks that cover [`Self::deadline`], at least
    /// one.
    pub(crate) fn deadline_ticks(&self) -> u64 {
        let tick = EPOCH_TICK.as_nanos().max(1);
        let wanted = self.deadline.as_nanos();
        let ticks = wanted.div_ceil(tick);
        u64::try_from(ticks).unwrap_or(u64::MAX).max(1)
    }
}

/// The store's resource limiter plus the record of a refused growth.
///
/// Wasmtime reports a refused `memory.grow` to the guest as `-1`, not as
/// a trap. The guest is free to handle that; when it instead traps (the
/// fixture does), this flag is what lets the host classify the trap as a
/// memory-limit failure rather than a generic `unreachable`.
#[derive(Debug)]
pub(crate) struct StoreState {
    max_memory_bytes: usize,
    memory_denied: bool,
}

impl StoreState {
    pub(crate) const fn new(max_memory_bytes: usize) -> Self {
        Self {
            max_memory_bytes,
            memory_denied: false,
        }
    }

    /// Clear the per-call record before arming a call.
    pub(crate) fn reset_for_call(&mut self) {
        self.memory_denied = false;
    }

    /// Take the refusal flag, resetting it for the next call.
    pub(crate) fn take_memory_denied(&mut self) -> bool {
        std::mem::take(&mut self.memory_denied)
    }
}

impl ResourceLimiter for StoreState {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> Result<bool, Error> {
        if desired > self.max_memory_bytes {
            self.memory_denied = true;
            return Ok(false);
        }
        Ok(true)
    }

    fn memory_grow_failed(&mut self, _error: Error) -> Result<(), Error> {
        // The limiter allowed the growth but the host allocator could not
        // satisfy it. The guest sees the same `-1`; record it so a
        // subsequent trap is classified the same way.
        self.memory_denied = true;
        Ok(())
    }

    fn table_growing(
        &mut self,
        _current: usize,
        _desired: usize,
        _maximum: Option<usize>,
    ) -> Result<bool, Error> {
        Ok(true)
    }
}
