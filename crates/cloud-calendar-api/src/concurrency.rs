//! One execution budget for accounts and their nested calendar/week requests.
use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};

const PARALLEL: usize = 8;
static AVAILABLE: Mutex<usize> = Mutex::new(PARALLEL);
static READY: Condvar = Condvar::new();
thread_local! {
    static WORKER: Cell<bool> = const { Cell::new(false) };
}

pub(crate) struct Permit;

impl Drop for Permit {
    fn drop(&mut self) {
        *AVAILABLE.lock().unwrap() += 1;
        READY.notify_one();
    }
}

/// A nested request already owns its account worker's slot.
pub(crate) fn permit() -> Option<Permit> {
    if WORKER.get() {
        return None;
    }
    let mut available = AVAILABLE.lock().unwrap();
    while *available == 0 {
        available = READY.wait(available).unwrap();
    }
    *available -= 1;
    Some(Permit)
}

fn try_permit() -> Option<Permit> {
    let mut available = AVAILABLE.lock().unwrap();
    if *available == 0 {
        return None;
    }
    *available -= 1;
    Some(Permit)
}

/// Preserve input order and per-item failures, with no thread for every item.
/// Nested maps reuse their worker and borrow spare slots without waiting for them.
pub(crate) fn map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<std::thread::Result<R>> {
    let nested = WORKER.get();
    let next = AtomicUsize::new(0);
    let results = Mutex::new((0..items.len()).map(|_| None).collect::<Vec<_>>());
    let run = || {
        WORKER.set(true);
        loop {
            let index = next.fetch_add(1, Ordering::Relaxed);
            let Some(item) = items.get(index) else { break };
            let result = catch_unwind(AssertUnwindSafe(|| f(item)));
            results.lock().unwrap()[index] = Some(result);
        }
    };
    std::thread::scope(|scope| {
        if nested {
            // The caller owns one slot. Never wait for helpers: other account
            // workers may already own every remaining slot.
            for _ in 1..items.len().min(PARALLEL) {
                let Some(permit) = try_permit() else { break };
                let run = &run;
                scope.spawn(move || {
                    let _permit = permit;
                    run();
                });
            }
            run();
        } else {
            for _ in 0..items.len().min(PARALLEL) {
                scope.spawn(|| {
                    let _permit = permit();
                    run();
                });
            }
        }
    });
    results.into_inner().unwrap().into_iter().map(Option::unwrap).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    static TESTS: Mutex<()> = Mutex::new(());

    #[test]
    fn nested_accounts_share_the_budget_and_preserve_order() {
        let _test = TESTS.lock().unwrap();
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let accounts: Vec<_> = (0..20).collect();
        let weeks: Vec<_> = (0..12).collect();
        let run = || {
            map(&accounts, |account| {
                map(&weeks, |week| {
                    // Subprocess execution reuses a nested account's permit.
                    let _permit = permit();
                    let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(current, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(1));
                    active.fetch_sub(1, Ordering::SeqCst);
                    account * 100 + week
                })
                .into_iter()
                .map(Result::unwrap)
                .collect::<Vec<_>>()
            })
            .into_iter()
            .map(Result::unwrap)
            .collect::<Vec<_>>()
        };
        // Separate UI requests must also share the same process-wide limit.
        let (first, second) = std::thread::scope(|scope| {
            let first = scope.spawn(run);
            (run(), first.join().unwrap())
        });
        let expected: Vec<Vec<_>> = accounts
            .iter()
            .map(|a| weeks.iter().map(|w| a * 100 + w).collect())
            .collect();
        assert_eq!(first, expected);
        assert_eq!(second, expected);
        assert!(peak.load(Ordering::SeqCst) <= PARALLEL);
        assert!(peak.load(Ordering::SeqCst) > 1);
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn one_account_still_fetches_its_weeks_in_parallel() {
        let _test = TESTS.lock().unwrap();
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let weeks: Vec<_> = (0..20).collect();
        let results = map(&[0], |_| {
            map(&weeks, |week| {
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(current, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(1));
                active.fetch_sub(1, Ordering::SeqCst);
                *week
            })
            .into_iter()
            .map(Result::unwrap)
            .collect::<Vec<_>>()
        });
        assert_eq!(results.into_iter().next().unwrap().unwrap(), weeks);
        assert!(peak.load(Ordering::SeqCst) > 1);
        assert!(peak.load(Ordering::SeqCst) <= PARALLEL);
    }

    #[test]
    fn a_failed_item_does_not_drop_later_items_or_leak_the_budget() {
        let _test = TESTS.lock().unwrap();
        let results = map(&[0, 1, 2], |n| {
            assert_ne!(*n, 1, "provider failed unexpectedly");
            n * 10
        });
        assert_eq!(*results[0].as_ref().unwrap(), 0);
        assert!(results[1].is_err());
        assert_eq!(*results[2].as_ref().unwrap(), 20);
        assert_eq!(map(&[3], |n| n * 10).pop().unwrap().unwrap(), 30);
        assert!(map::<u8, u8>(&[], |n| *n).is_empty());
    }
}
