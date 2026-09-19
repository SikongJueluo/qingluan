// Throwaway probe-local quota state machine (Reserved -> Active -> Cleaning ->
// Released), validating docs/design/agent-terminal.md rules:
// - Only Released frees a slot; Reserved/Active/Cleaning all occupy.
// - Session and global limits are checked atomically (no dangling reservation).
// - Each slot is released exactly once.
// NOT production code.

use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotState {
    Reserved,
    Active,
    Cleaning,
    Released,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuotaError {
    SessionExhausted,
    GlobalExhausted,
    UnknownSlot,
    AlreadyReleased,
    IllegalTransition(SlotState),
}

struct Inner {
    session_counts: HashMap<String, usize>,
    global_count: usize,
    slots: HashMap<u64, (String, SlotState)>,
    next_id: u64,
}

impl Inner {
    fn occupying(&self) -> usize {
        self.global_count
    }
    fn session_occupying(&self, session: &str) -> usize {
        self.session_counts.get(session).copied().unwrap_or(0)
    }
}

pub struct Quota {
    session_limit: usize,
    global_limit: usize,
    inner: Mutex<Inner>,
}

impl Quota {
    pub fn new(session_limit: usize, global_limit: usize) -> Self {
        Self {
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

    /// Atomically reserve one slot against both limits. On failure nothing is
    /// left behind on either side.
    pub fn reserve(&self, session_id: &str) -> Result<u64, QuotaError> {
        let mut inner = self.inner.lock().unwrap();
        if inner.session_occupying(session_id) >= self.session_limit {
            return Err(QuotaError::SessionExhausted);
        }
        if inner.occupying() >= self.global_limit {
            return Err(QuotaError::GlobalExhausted);
        }
        let id = inner.next_id;
        inner.next_id += 1;
        inner
            .session_counts
            .entry(session_id.to_string())
            .and_modify(|c| *c += 1)
            .or_insert(1);
        inner.global_count += 1;
        inner
            .slots
            .insert(id, (session_id.to_string(), SlotState::Reserved));
        Ok(id)
    }

    /// Reserved -> Active (PTY created, root started).
    pub fn internalize(&self, id: u64) -> Result<(), QuotaError> {
        self.transition(id, |state| match state {
            SlotState::Reserved => Ok(SlotState::Active),
            other => Err(other),
        })
    }

    /// Reserved -> Cleaning -> Released (start failed; no terminal ever ran).
    pub fn start_failed(&self, id: u64) -> Result<(), QuotaError> {
        self.transition(id, |state| match state {
            SlotState::Reserved => Ok(SlotState::Cleaning),
            other => Err(other),
        })?;
        self.release(id)
    }

    /// Reserved/Active -> Cleaning (stop committed / cleanup started). Used
    /// by start_failed (Reserved -> Cleaning -> Released) and by the startup
    /// rollback when cleanup could NOT be verified: a Cleaning slot without
    /// a later release stays occupied, so an unverified cleanup is loud
    /// rather than silently freeing the slot.
    pub fn begin_cleaning(&self, id: u64) -> Result<(), QuotaError> {
        self.transition(id, |state| match state {
            SlotState::Reserved | SlotState::Active => Ok(SlotState::Cleaning),
            other => Err(other),
        })
    }

    /// Cleaning -> Released. Releasing an already-released slot is an error
    /// (each slot frees exactly once).
    pub fn release(&self, id: u64) -> Result<(), QuotaError> {
        self.transition(id, |state| {
            match state {
                SlotState::Cleaning => Ok(SlotState::Released),
                SlotState::Released => Ok(SlotState::Released), // detected in transition
                other => Err(other),
            }
        })
    }

    fn transition(
        &self,
        id: u64,
        f: impl FnOnce(SlotState) -> Result<SlotState, SlotState>,
    ) -> Result<(), QuotaError> {
        let mut inner = self.inner.lock().unwrap();
        let (session, before) = {
            let Some((session, state)) = inner.slots.get(&id) else {
                return Err(QuotaError::UnknownSlot);
            };
            (session.clone(), *state)
        };
        let after = f(before).map_err(QuotaError::IllegalTransition)?;
        if before == SlotState::Released {
            return Err(QuotaError::AlreadyReleased);
        }
        if after == SlotState::Released {
            let count = inner
                .session_counts
                .get_mut(&session)
                .expect("session count exists while slot occupies");
            *count -= 1;
            if *count == 0 {
                inner.session_counts.remove(&session);
            }
            inner.global_count -= 1;
        }
        if let Some((_, state)) = inner.slots.get_mut(&id) {
            *state = after;
        }
        Ok(())
    }

    pub fn state(&self, id: u64) -> Option<SlotState> {
        self.inner.lock().unwrap().slots.get(&id).map(|(_, s)| *s)
    }

    pub fn occupying(&self) -> usize {
        self.inner.lock().unwrap().occupying()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_cycle_releases_once() {
        let quota = Quota::new(2, 3);
        let slot = quota.reserve("s1").unwrap();
        assert_eq!(quota.occupying(), 1);
        quota.internalize(slot).unwrap();
        quota.begin_cleaning(slot).unwrap();
        quota.release(slot).unwrap();
        assert_eq!(quota.occupying(), 0);
        assert_eq!(quota.state(slot), Some(SlotState::Released));
        // Second release must be rejected: each slot frees exactly once.
        assert_eq!(quota.release(slot), Err(QuotaError::AlreadyReleased));
        assert_eq!(quota.occupying(), 0);
    }

    #[test]
    fn cleaning_without_release_stays_occupied() {
        let quota = Quota::new(1, 2);
        let slot = quota.reserve("s1").unwrap();
        // Unverified cleanup (startup rollback failure): Reserved -> Cleaning
        // with NO release keeps the slot occupied and blocks its session.
        quota.begin_cleaning(slot).unwrap();
        assert_eq!(quota.state(slot), Some(SlotState::Cleaning));
        assert_eq!(quota.occupying(), 1);
        assert_eq!(
            quota.reserve("s1").unwrap_err(),
            QuotaError::SessionExhausted
        );
        assert!(quota.reserve("s2").is_ok());
    }

    #[test]
    fn start_failure_frees_slot() {
        let quota = Quota::new(2, 3);
        let slot = quota.reserve("s1").unwrap();
        quota.start_failed(slot).unwrap();
        assert_eq!(quota.state(slot), Some(SlotState::Released));
        assert_eq!(quota.occupying(), 0);
        // Slot usable again after a failed start.
        assert!(quota.reserve("s1").is_ok());
    }

    #[test]
    fn limits_are_atomic_per_side() {
        let quota = Quota::new(1, 10);
        assert!(quota.reserve("s1").is_ok());
        assert_eq!(
            quota.reserve("s1").unwrap_err(),
            QuotaError::SessionExhausted
        );
        // Session side must not be left dangling: a failure reserves nothing.
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
                        // Simulated start failure.
                        quota.start_failed(slot).expect("start_failed");
                    } else {
                        quota.internalize(slot).expect("internalize");
                        quota.begin_cleaning(slot).expect("begin_cleaning");
                        quota.release(slot).expect("release");
                    }
                }
            }));
        }
        for handle in handles {
            handle.await.expect("task");
        }
        assert_eq!(quota.occupying(), 0, "no slot may stay occupied at the end");
    }
}
