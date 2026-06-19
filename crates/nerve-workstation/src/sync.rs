//! Lock-poisoning recovery helpers.
//!
//! A panicking thread must not take down the whole server. These thin wrappers
//! recover the inner guard when a lock is poisoned, letting unrelated threads
//! continue. Happy-path behaviour is identical to `.unwrap()`.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Acquire a mutex lock, recovering the guard from a poisoned lock.
pub fn lock_recover<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}
