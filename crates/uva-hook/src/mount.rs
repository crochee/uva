//! The twelve mount points and the lane a registration runs in.
//!
//! [`MountPoint`] is the closed vocabulary of where an interception
//! may be registered. Six of its variants are the pass points the
//! loop's kernel waterfalls dispatch (ADR 0004); five are the
//! observation points a run reports through; the last is the
//! system-prompt collection. Every variant declares exactly one wire
//! spelling in [`MountPoint::as_str`], and [`MountPoint::parse`] reads
//! that same declaration, so a point cannot have two names.
//!
//! The observation points keep the spelling their event already has
//! (`before_run`, `after_run`, …): that is the name the vocabulary and
//! the run's diagnostics use, and a second name for the same point
//! would be exactly the drift this enum removes. The six pass points
//! keep their kernel waterfall keys, and the collection its
//! `prompt/sections` key.
//!
//! [`RegistrationOrigin`] says who registered: an out-of-tree plugin
//! or the deployment's in-process policy. The origin decides the lane
//! a registration runs in at a point (plugins first, then in-process,
//! registration order inside a lane) and what happens when the
//! registration panics (durable plugins are kept, in-process policy is
//! withdrawn). Both rules are declared here once.

use crate::arbitration::PanicPolicy;

/// One of the twelve places interception is asked.
///
/// The enum is closed: adding a mount point forces every exhaustive
/// match to be visited, so a new point cannot be silently dropped by a
/// dispatcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MountPoint {
    /// `agent/step-pre` — before a loop step samples the model.
    StepPre,
    /// `agent/step-post` — after a loop step committed its results.
    StepPost,
    /// `tool/pre-execute` — before a tool call executes.
    ToolPre,
    /// `tool/post-execute` — after a tool returned, before commit.
    ToolPost,
    /// `llm/request` — before a model request goes on the wire.
    LlmRequest,
    /// `llm/result` — after a model request returned.
    LlmResult,
    /// `before_run` — once per run, before the first step.
    BeforeRun,
    /// `after_run` — once per run, after the last step.
    AfterRun,
    /// `before_provider_call` — right before the provider dispatch.
    BeforeProviderCall,
    /// `on_error` — the run ends with an error.
    OnError,
    /// `on_compaction` — a compaction happened.
    OnCompaction,
    /// `prompt/sections` — the system-prompt collection.
    PromptSections,
}

impl MountPoint {
    /// Every mount point: the six pass points, the five observation
    /// points, then the collection.
    pub const ALL: [Self; 12] = [
        Self::StepPre,
        Self::StepPost,
        Self::ToolPre,
        Self::ToolPost,
        Self::LlmRequest,
        Self::LlmResult,
        Self::BeforeRun,
        Self::AfterRun,
        Self::BeforeProviderCall,
        Self::OnError,
        Self::OnCompaction,
        Self::PromptSections,
    ];

    /// The point's one wire spelling.
    ///
    /// [`Self::parse`] reads this same declaration; a spelling is
    /// never written down twice.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StepPre => "agent/step-pre",
            Self::StepPost => "agent/step-post",
            Self::ToolPre => "tool/pre-execute",
            Self::ToolPost => "tool/post-execute",
            Self::LlmRequest => "llm/request",
            Self::LlmResult => "llm/result",
            Self::BeforeRun => "before_run",
            Self::AfterRun => "after_run",
            Self::BeforeProviderCall => "before_provider_call",
            Self::OnError => "on_error",
            Self::OnCompaction => "on_compaction",
            Self::PromptSections => "prompt/sections",
        }
    }

    /// The point spelled by `wire`, if any.
    #[must_use]
    pub fn parse(wire: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mount| mount.as_str() == wire)
    }

    /// Whether this point dispatches a pass: a registration may
    /// observe, rewrite or short-circuit there.
    #[must_use]
    pub const fn is_pass(self) -> bool {
        matches!(
            self,
            Self::StepPre
                | Self::StepPost
                | Self::ToolPre
                | Self::ToolPost
                | Self::LlmRequest
                | Self::LlmResult
        )
    }

    /// Whether this point only observes: a registration may report,
    /// but the point does not carry a rewrite of its own.
    #[must_use]
    pub const fn is_observation(self) -> bool {
        matches!(
            self,
            Self::BeforeRun
                | Self::AfterRun
                | Self::BeforeProviderCall
                | Self::OnError
                | Self::OnCompaction
        )
    }

    /// Whether this point is the prompt-section collection.
    #[must_use]
    pub const fn is_sections(self) -> bool {
        matches!(self, Self::PromptSections)
    }

    /// Whether a registration here dispatches over a [`HookEvent`].
    ///
    /// [`HookEvent`]: uva_steering::HookEvent
    #[must_use]
    pub const fn is_event(self) -> bool {
        !self.is_sections()
    }
}

impl std::fmt::Display for MountPoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Who registered an interception.
///
/// The origin is a fact on the registration, not something a receiver
/// infers from which entry point was used: it decides the lane the
/// registration runs in and what a panic does to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrationOrigin {
    /// An out-of-tree plugin registration.
    Plugin,
    /// The deployment's in-process policy.
    InProcess,
}

impl RegistrationOrigin {
    /// The lane at a mount point.
    ///
    /// Plugin registrations run before in-process ones; inside a lane
    /// the registration order decides. The lane is a field on the
    /// registration, not a second dispatch path.
    pub(crate) const fn lane(self) -> u8 {
        match self {
            Self::Plugin => 0,
            Self::InProcess => 1,
        }
    }

    /// What a panic does to a registration from this origin.
    ///
    /// A plugin may fail transiently and is not told it was disabled,
    /// so it is skipped and kept. An in-process registration that
    /// panics is broken, so it is withdrawn and the fact says so.
    #[must_use]
    pub const fn panic_policy(self) -> PanicPolicy {
        match self {
            Self::Plugin => PanicPolicy::Keep,
            Self::InProcess => PanicPolicy::Withdraw,
        }
    }
}

#[cfg(test)]
mod tests;
