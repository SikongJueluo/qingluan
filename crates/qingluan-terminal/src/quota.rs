//! Per-terminal quota state machine.
//!
//! `Reserved -> Active -> Cleaning -> Released`, with `Reserved ->
//! Cleaning -> Released` for a failed start. Verified rules (report §5):
//! only `Released` frees a slot — `Reserved`, `Active`, and `Cleaning` all
//! occupy; the session and global limits are checked atomically (a refused
//! reservation leaves nothing behind); each slot is released exactly once;
//! an unverified cleanup leaves the slot in `Cleaning`, so a failed
//! rollback is loud rather than silently freeing capacity.
//!
//! Session/global limits are counts of *occupying* slots, so an unverified
//! cleanup correctly still blocks its session. The slot id is
//! process-local; correlation with the durable [`RuntimeRegistry`] record
//! is by terminal identity at the runtime slice, not by this id.
//!
//! [`RuntimeRegistry`]: qingluan_storage::RuntimeRegistry

use std::collections::HashMap;
use std::sync::Mutex;

/// Lifecycle phase of one quota slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SlotState {
    /// Reserved; the terminal is starting.
    Reserved,
    /// The root process is confirmed started.
    Active,
    /// Cleaning after a stop or a failed start; still occupies.
    Cleaning,
    /// Released; frees the slot.
    Released,
}

/// Why a quota operation was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QuotaError {
    /// The session is at its activity limit.
    SessionExhausted,
    /// The global activity limit is reached.
    GlobalExhausted,
    /// No slot with the given id exists.
    UnknownSlot,
    /// The slot was already released; a slot is released exactly once.
    AlreadyReleased,
    /// The requested transition is not legal from the current state.
    IllegalTransition(SlotState),
}

/// Process-local identifier of one reserved slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct SlotId(u64);

struct Inner {
    session_counts: HashMap<String, usize>,
    global_count: usize,
    slots: HashMap<u64, (String, SlotState)>,
    next_id: u64,
}

/// Two-level (session + global) activity quota.
pub(crate) struct Quota {
    session_limit: usize,
    global_limit: usize,
    inner: Mutex<Inner>,
}

impl Quota {
    /// Create a quota with the given session and global limits.
    pub(crate) fn new(session_limit: usize, global_limit: usize) -> Self {
        Quota {
            session_limit,
            global_limit,
            inner: Mutex::new(Inner {
                session_counts: HashMap::new(),
                global_count: 0,
                slots: HashMap::new(),
                next_id: 1,
            }),
        }
    }

    /// Atomically reserve one slot against both limits; on failure nothing
    /// is left behind on either side.
    pub(crate) fn reserve(&self, session_id: &str) -> Result<SlotId, QuotaError> {
        let mut inner = self.inner.lock().expect("quota lock");
        if inner.session_occupying(session_id) >= self.session_limit {
            return Err(QuotaError::SessionExhausted);
        }
        if inner.global_count >= self.global_limit {
            return Err(QuotaError::GlobalExhausted);
        }
        let id = inner.next_id;
        inner.next_id += 1;
        *inner
            .session_counts
            .entry(session_id.to_owned())
            .or_insert(0) += 1;
        inner.global_count += 1;
        inner
            .slots
            .insert(id, (session_id.to_owned(), SlotState::Reserved));
        Ok(SlotId(id))
    }

    /// `Reserved -> Active` (PTY created, root process started).
    pub(crate) fn activate(&self, id: SlotId) -> Result<(), QuotaError> {
        self.transition(id, |state| match state {
            SlotState::Reserved => Ok(SlotState::Active),
            other => Err(other),
        })
    }

    /// `Reserved`/`Active` -> `Cleaning` (stop committed or cleanup
    /// started). A `Cleaning` slot without a later release stays occupied,
    /// so an unverified cleanup blocks its session.
    pub(crate) fn begin_cleaning(&self, id: SlotId) -> Result<(), QuotaError> {
        self.transition(id, |state| match state {
            SlotState::Reserved | SlotState::Active => Ok(SlotState::Cleaning),
            other => Err(other),
        })
    }

    /// `Reserved -> Cleaning -> Released` for a failed start.
    pub(crate) fn start_failed(&self, id: SlotId) -> Result<(), QuotaError> {
        self.begin_cleaning(id)?;
        self.release(id)
    }

    /// `Cleaning -> Released`; releasing an already-released slot is an
    /// error (each slot frees exactly once).
    pub(crate) fn release(&self, id: SlotId) -> Result<(), QuotaError> {
        self.transition(id, |state| match state {
            SlotState::Cleaning => Ok(SlotState::Released),
            SlotState::Released => Ok(SlotState::Released),
            other => Err(other),
        })
    }

    /// Current state of a slot, if it exists.
    pub(crate) fn state(&self, id: SlotId) -> Option<SlotState> {
        self.inner
            .lock()
            .expect("quota lock")
            .slots
            .get(&id.0)
            .map(|(_, state)| *state)
    }

    /// Number of occupying (non-released) slots, global.
    pub(crate) fn occupying(&self) -> usize {
        self.inner.lock().expect("quota lock").global_count
    }

    fn transition(
        &self,
        id: SlotId,
        next: impl FnOnce(SlotState) -> Result<SlotState, SlotState>,
    ) -> Result<(), QuotaError> {
        let mut inner = self.inner.lock().expect("quota lock");
        let (session, before) = {
            let Some((session, state)) = inner.slots.get(&id.0) else {
                return Err(QuotaError::UnknownSlot);
            };
            (session.clone(), *state)
        };
        if before == SlotState::Released {
            return Err(QuotaError::AlreadyReleased);
        }
        let after = next(before).map_err(QuotaError::IllegalTransition)?;
        if after == SlotState::Released {
            let count = inner
                .session_counts
                .get_mut(&session)
                .expect("a session count exists while its slot occupies");
            *count -= 1;
            if *count == 0 {
                inner.session_counts.remove(&session);
            }
            inner.global_count -= 1;
        }
        if let Some((_, state)) = inner.slots.get_mut(&id.0) {
            *state = after;
        }
        Ok(())
    }
}

impl Inner {
    fn session_occupying(&self, session: &str) -> usize {
        self.session_counts.get(session).copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_cycle_releases_exactly_once() {
        let quota = Quota::new(2, 3);
        let slot = quota.reserve("s1").unwrap();
        assert_eq!(quota.occupying(), 1);
        quota.activate(slot).unwrap();
        quota.begin_cleaning(slot).unwrap();
        quota.release(slot).unwrap();
        assert_eq!(quota.occupying(), 0);
        assert_eq!(quota.state(slot), Some(SlotState::Released));
        // A second release must be rejected: each slot frees exactly once.
        assert_eq!(quota.release(slot), Err(QuotaError::AlreadyReleased));
        assert_eq!(quota.occupying(), 0);
    }

    #[test]
    fn start_failure_frees_the_slot_and_cleaning_without_release_occupies() {
        let quota = Quota::new(1, 2);
        let slot = quota.reserve("s1").unwrap();
        quota.start_failed(slot).unwrap();
        assert_eq!(quota.state(slot), Some(SlotState::Released));
        assert_eq!(quota.occupying(), 0);

        // An unverified cleanup (Cleaning with no release) keeps the slot
        // occupied and blocks its session.
        let slot = quota.reserve("s1").unwrap();
        quota.begin_cleaning(slot).unwrap();
        assert_eq!(quota.state(slot), Some(SlotState::Cleaning));
        assert_eq!(quota.occupying(), 1);
        assert_eq!(
            quota.reserve("s1").unwrap_err(),
            QuotaError::SessionExhausted
        );
    }

    #[test]
    fn limits_are_atomic_per_side() {
        let quota = Quota::new(1, 10);
        assert!(quota.reserve("s1").is_ok());
        assert_eq!(
            quota.reserve("s1").unwrap_err(),
            QuotaError::SessionExhausted
        );
        // The refused reservation leaves the session count unchanged.
        assert_eq!(quota.occupying(), 1);
        assert!(quota.reserve("s2").is_ok());

        let quota = Quota::new(10, 2);
        quota.reserve("s1").unwrap();
        quota.reserve("s2").unwrap();
        assert_eq!(
            quota.reserve("s3").unwrap_err(),
            QuotaError::GlobalExhausted
        );
        assert_eq!(quota.occupying(), 2);
    }

    #[test]
    fn illegal_transitions_are_refused() {
        let quota = Quota::new(2, 2);
        let slot = quota.reserve("s1").unwrap();
        // Reserved cannot be released directly.
        assert_eq!(
            quota.release(slot),
            Err(QuotaError::IllegalTransition(SlotState::Reserved))
        );
        quota.activate(slot).unwrap();
        // Active cannot be activated again.
        assert_eq!(
            quota.activate(slot),
            Err(QuotaError::IllegalTransition(SlotState::Active))
        );
        assert_eq!(quota.state(SlotId(999)), None,);
    }

    #[tokio::test]
    async fn concurrent_reserve_release_races_stay_consistent() {
        let quota = std::sync::Arc::new(Quota::new(4, 8));
        let mut handles = Vec::new();
        for task in 0..16u32 {
            let quota = quota.clone();
            handles.push(tokio::spawn(async move {
                for round in 0..12u32 {
                    let session = format!("s{}", task % 3);
                    let Ok(slot) = quota.reserve(&session) else {
                        continue;
                    };
                    if (round + task) % 3 == 0 {
                        quota.start_failed(slot).expect("start_failed");
                    } else {
                        quota.activate(slot).expect("activate");
                        quota.begin_cleaning(slot).expect("begin_cleaning");
                        quota.release(slot).expect("release");
                    }
                }
            }));
        }
        for handle in handles {
            handle.await.expect("task");
        }
        assert_eq!(quota.occupying(), 0, "no slot may stay occupied");
    }
}
