//! Atomic accounting shared by loaded SQL results. Reservations follow result lifetimes.
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

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
    bytes: usize,
}

impl Reservation {
    pub fn new(budget: Arc<ResultBudget>) -> Self {
        Self { budget, bytes: 0 }
    }
    pub fn grow(&mut self, bytes: usize) -> bool {
        let limit = self.budget.limit.load(Ordering::Relaxed);
        if self
            .budget
            .used
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                used.checked_add(bytes).filter(|&next| next <= limit)
            })
            .is_err()
        {
            return false;
        }
        self.bytes += bytes;
        true
    }
}

impl Default for Reservation {
    fn default() -> Self {
        Self::new(ResultBudget::new(usize::MAX))
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.budget.used.fetch_sub(self.bytes, Ordering::Relaxed);
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
                    let mut reservation = Reservation::new(budget);
                    if reservation.grow(100) {
                        successes.fetch_add(1, Ordering::Relaxed);
                    }
                    attempted.wait();
                    release.wait();
                });
            }
            attempted.wait();
            let accepted = successes.load(Ordering::Relaxed);
            release.wait();
            assert_eq!(accepted, 1);
        });
        assert!(
            Reservation::new(budget).grow(100),
            "dropping results frees their reservations"
        );
    }
}
