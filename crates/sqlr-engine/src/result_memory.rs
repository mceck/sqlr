//! Atomic accounting shared by loaded SQL results. Reservations follow chunk lifetimes.
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Debug)]
pub(crate) struct ResultBudget {
    pub limit: AtomicUsize,
    used: AtomicUsize,
}

impl ResultBudget {
    pub fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            limit: AtomicUsize::new(limit),
            used: AtomicUsize::new(0),
        })
    }
}

pub(crate) struct Reservation {
    budget: Arc<ResultBudget>,
    used: Arc<AtomicUsize>,
}

impl Reservation {
    pub fn new(budget: Arc<ResultBudget>) -> Self {
        Self {
            budget,
            used: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn bytes(&self) -> usize {
        self.used.load(Ordering::Relaxed)
    }

    pub fn reserve(&self, bytes: usize) -> Option<Arc<ChunkReservation>> {
        let limit = self.budget.limit.load(Ordering::Relaxed);
        self.budget
            .used
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                used.checked_add(bytes).filter(|&next| next <= limit)
            })
            .ok()?;
        self.used.fetch_add(bytes, Ordering::Relaxed);
        Some(Arc::new(ChunkReservation {
            budget: self.budget.clone(),
            used: self.used.clone(),
            bytes,
        }))
    }
}

impl Default for Reservation {
    fn default() -> Self {
        Self::new(ResultBudget::new(usize::MAX))
    }
}

#[derive(Debug)]
pub(crate) struct ChunkReservation {
    budget: Arc<ResultBudget>,
    used: Arc<AtomicUsize>,
    bytes: usize,
}

impl Drop for ChunkReservation {
    fn drop(&mut self) {
        self.budget.used.fetch_sub(self.bytes, Ordering::Relaxed);
        self.used.fetch_sub(self.bytes, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;

    #[test]
    fn concurrent_results_cannot_overbook_the_global_budget() {
        let budget = ResultBudget::new(100);
        let attempted = Arc::new(Barrier::new(3));
        let release = Arc::new(Barrier::new(3));
        let successes = Arc::new(AtomicUsize::new(0));
        std::thread::scope(|threads| {
            for _ in 0..2 {
                let budget = budget.clone();
                let attempted = attempted.clone();
                let release = release.clone();
                let successes = successes.clone();
                threads.spawn(move || {
                    let reservation = Reservation::new(budget);
                    let lease = reservation.reserve(100);
                    if lease.is_some() {
                        successes.fetch_add(1, Ordering::Relaxed);
                    }
                    attempted.wait();
                    release.wait();
                    drop(lease);
                });
            }
            attempted.wait();
            let accepted = successes.load(Ordering::Relaxed);
            release.wait();
            assert_eq!(accepted, 1);
        });
        assert!(
            Reservation::new(budget).reserve(100).is_some(),
            "dropping results frees their reservations"
        );
    }
}
