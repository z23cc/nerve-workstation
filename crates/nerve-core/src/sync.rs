//! Lock-poisoning recovery helpers.
//!
//! A panicking thread must not take down the whole server. These thin wrappers
//! recover the inner guard when a lock is poisoned, letting unrelated threads
//! continue. Happy-path behaviour is identical to `.unwrap()`.

use std::sync::{Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// Acquire a mutex lock, recovering the guard from a poisoned lock.
pub fn lock_recover<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Acquire an RwLock read guard, recovering from a poisoned lock.
pub fn read_recover<T>(rw: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    rw.read().unwrap_or_else(PoisonError::into_inner)
}

/// Acquire an RwLock write guard, recovering from a poisoned lock.
pub fn write_recover<T>(rw: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    rw.write().unwrap_or_else(PoisonError::into_inner)
}
