use serde::{Deserialize, Serialize};

/// Ticket states. See docs/design/01-flow-and-state-machine.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    New,
    NeedsInfo,
    DuplicateProposed,
    Triaged,
    AwaitingApproval,
    Queued,
    Developing,
    Rescoring,
    PrOpen,
    CiRunning,
    Reviewing,
    MergeReady,
    Merged,
    Failed,
    Stopped,
    ClosedStale,
    ClosedDuplicate,
    ClosedRejected,
    ClosedDone,
}

impl State {
    pub const ALL: [State; 19] = [
        New,
        NeedsInfo,
        DuplicateProposed,
        Triaged,
        AwaitingApproval,
        Queued,
        Developing,
        Rescoring,
        PrOpen,
        CiRunning,
        Reviewing,
        MergeReady,
        Merged,
        Failed,
        Stopped,
        ClosedStale,
        ClosedDuplicate,
        ClosedRejected,
        ClosedDone,
    ];

    /// The snake_case name stored in the database.
    pub fn as_str(self) -> &'static str {
        match self {
            New => "new",
            NeedsInfo => "needs_info",
            DuplicateProposed => "duplicate_proposed",
            Triaged => "triaged",
            AwaitingApproval => "awaiting_approval",
            Queued => "queued",
            Developing => "developing",
            Rescoring => "rescoring",
            PrOpen => "pr_open",
            CiRunning => "ci_running",
            Reviewing => "reviewing",
            MergeReady => "merge_ready",
            Merged => "merged",
            Failed => "failed",
            Stopped => "stopped",
            ClosedStale => "closed_stale",
            ClosedDuplicate => "closed_duplicate",
            ClosedRejected => "closed_rejected",
            ClosedDone => "closed_done",
        }
    }

    pub fn parse(s: &str) -> Option<State> {
        State::ALL.into_iter().find(|st| st.as_str() == s)
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            State::ClosedStale | State::ClosedDuplicate | State::ClosedRejected | State::ClosedDone
        )
    }
}

/// Who asks for a transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Actor {
    /// The orchestrator, acting on agent results and admission control.
    Orchestrator,
    /// The PR runner or the merge runner.
    Runner,
    /// A human with an approver role (console, ticket source or chat button).
    Approver,
    /// The person who reported the problem (a reply on the ticket).
    Reporter,
    /// The admin console stop / resume switch.
    Console,
    /// A clock. It may close things as not done, and nothing else.
    Timer,
}

use Actor::*;
use State::*;

/// Every allowed edge and who may take it.
const EDGES: &[(State, State, &[Actor])] = &[
    (New, Triaged, &[Orchestrator]),
    (New, NeedsInfo, &[Orchestrator]),
    (New, DuplicateProposed, &[Orchestrator]),
    (NeedsInfo, New, &[Reporter]),
    (NeedsInfo, ClosedStale, &[Timer]),
    (DuplicateProposed, ClosedDuplicate, &[Approver]),
    (DuplicateProposed, Triaged, &[Approver]),
    // Not a duplicate after all: the ticket is triaged normally.
    (DuplicateProposed, New, &[Approver]),
    // Triaged -> Queued is only for level 1; see `transition_checked` for the level guard.
    (Triaged, Queued, &[Orchestrator]),
    (Triaged, AwaitingApproval, &[Orchestrator]),
    (AwaitingApproval, Queued, &[Approver]),
    (AwaitingApproval, ClosedRejected, &[Approver]),
    (AwaitingApproval, ClosedStale, &[Timer]),
    (Queued, Developing, &[Orchestrator]),
    (Developing, Rescoring, &[Orchestrator]),
    (Developing, Failed, &[Orchestrator]),
    (Rescoring, PrOpen, &[Runner]),
    (Rescoring, AwaitingApproval, &[Runner]),
    (PrOpen, CiRunning, &[Runner]),
    (CiRunning, Reviewing, &[Orchestrator]),
    (CiRunning, Developing, &[Orchestrator]),
    (CiRunning, Failed, &[Orchestrator]),
    (Reviewing, MergeReady, &[Orchestrator]),
    (Reviewing, Developing, &[Orchestrator]),
    (Reviewing, AwaitingApproval, &[Orchestrator]),
    (MergeReady, Merged, &[Runner]),
    (Merged, ClosedDone, &[Approver]),
    // Approved work resumes where it stopped: a rescored diff goes on to its PR,
    // a reviewer escalation goes on to the merge, and a moved head is checked again.
    (Queued, Rescoring, &[Orchestrator]),
    (Queued, MergeReady, &[Orchestrator]),
    (MergeReady, CiRunning, &[Runner]),
    // A person with a say asked for changes on the pull request: the developer works on it again.
    (PrOpen, Developing, &[Orchestrator]),
    (MergeReady, Developing, &[Orchestrator]),
    (Failed, Queued, &[Approver]),
    (Failed, ClosedRejected, &[Approver]),
];

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TransitionError {
    #[error("{from:?} is terminal")]
    Terminal { from: State },
    #[error("no transition {from:?} -> {to:?}")]
    NoSuchEdge { from: State, to: State },
    #[error("{actor:?} may not move {from:?} -> {to:?}")]
    NotAllowed {
        from: State,
        to: State,
        actor: Actor,
    },
}

/// Check one transition. Stopping and resuming are the console's only
/// privileges; any non-terminal state can be stopped, and a stopped ticket
/// can be resumed into any non-terminal state.
pub fn transition(from: State, to: State, actor: Actor) -> Result<(), TransitionError> {
    if from.is_terminal() {
        return Err(TransitionError::Terminal { from });
    }
    if to == Stopped || from == Stopped {
        if actor == Console && !(from == Stopped && to == Stopped) && !to.is_terminal() {
            return Ok(());
        }
        return Err(TransitionError::NotAllowed { from, to, actor });
    }
    // The orchestrator may fail any live ticket whose retries ran out.
    if to == Failed && actor == Orchestrator && from != Failed {
        return Ok(());
    }
    match EDGES.iter().find(|(f, t, _)| *f == from && *t == to) {
        None => Err(TransitionError::NoSuchEdge { from, to }),
        Some((_, _, actors)) if actors.contains(&actor) => Ok(()),
        Some(_) => Err(TransitionError::NotAllowed { from, to, actor }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip_and_match_serde() {
        for s in State::ALL {
            assert_eq!(State::parse(s.as_str()), Some(s));
            assert_eq!(
                serde_json::to_value(s).unwrap(),
                serde_json::Value::String(s.as_str().into())
            );
        }
        assert_eq!(State::parse("nope"), None);
    }

    #[test]
    fn happy_path_is_allowed() {
        let path = [
            (New, Triaged, Orchestrator),
            (Triaged, AwaitingApproval, Orchestrator),
            (AwaitingApproval, Queued, Approver),
            (Queued, Developing, Orchestrator),
            (Developing, Rescoring, Orchestrator),
            (Rescoring, PrOpen, Runner),
            (PrOpen, CiRunning, Runner),
            (CiRunning, Reviewing, Orchestrator),
            (Reviewing, MergeReady, Orchestrator),
            (MergeReady, Merged, Runner),
            (Merged, ClosedDone, Approver),
        ];
        for (f, t, a) in path {
            assert_eq!(transition(f, t, a), Ok(()), "{f:?} -> {t:?} by {a:?}");
        }
    }

    #[test]
    fn a_timer_can_never_approve() {
        assert!(transition(AwaitingApproval, Queued, Timer).is_err());
    }

    #[test]
    fn a_timer_may_only_close_as_stale() {
        for (f, t, actors) in EDGES {
            if actors.contains(&Timer) {
                assert_eq!(*t, ClosedStale, "timer edge {f:?} -> {t:?}");
            }
        }
    }

    #[test]
    fn only_approvers_leave_awaiting_approval_towards_work() {
        for actor in [Orchestrator, Runner, Reporter, Timer, Console] {
            assert!(
                transition(AwaitingApproval, Queued, actor).is_err(),
                "{actor:?}"
            );
        }
    }

    #[test]
    fn agents_and_runners_cannot_skip_review_or_merge() {
        assert!(transition(CiRunning, MergeReady, Orchestrator).is_err());
        assert!(transition(Reviewing, Merged, Runner).is_err());
        assert!(transition(Developing, PrOpen, Orchestrator).is_err());
    }

    #[test]
    fn terminal_states_are_final() {
        for s in [ClosedStale, ClosedDuplicate, ClosedRejected, ClosedDone] {
            assert_eq!(
                transition(s, New, Orchestrator),
                Err(TransitionError::Terminal { from: s })
            );
            assert!(transition(s, Stopped, Console).is_err());
        }
    }

    #[test]
    fn only_the_orchestrator_can_fail_a_live_ticket() {
        for from in State::ALL {
            let live = !from.is_terminal() && from != Stopped && from != Failed;
            assert_eq!(
                transition(from, Failed, Orchestrator).is_ok(),
                live || from == Developing || from == CiRunning,
                "{from:?}"
            );
            if live {
                assert!(transition(from, Failed, Timer).is_err(), "{from:?}");
            }
        }
    }

    #[test]
    fn only_the_console_stops_and_resumes() {
        assert_eq!(transition(Developing, Stopped, Console), Ok(()));
        assert_eq!(transition(Stopped, Developing, Console), Ok(()));
        assert!(transition(Developing, Stopped, Orchestrator).is_err());
        assert!(transition(Stopped, Queued, Approver).is_err());
        assert!(transition(Stopped, ClosedDone, Console).is_err());
    }

    #[test]
    fn every_edge_leaves_a_non_terminal_state() {
        for (f, _, _) in EDGES {
            assert!(!f.is_terminal(), "{f:?}");
        }
    }
}
