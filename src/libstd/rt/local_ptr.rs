// Copyright 2013 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Access to a single thread-local pointer.
//!
//! The runtime will use this for storing ~Task.
//!
//! XXX: Add runtime checks for usage of inconsistent pointer types.
//! and for overwriting an existing pointer.

#[allow(dead_code)];

use cast;
use cell::Cell;
use unstable::finally::Finally;

#[cfg(windows)]               // mingw-w32 doesn't like thread_local things
#[cfg(target_os = "android")] // see #10686
pub use self::native::*;

#[cfg(not(windows), not(target_os = "android"))]
pub use self::compiled::*;

/// Borrow the thread-local value from thread-local storage.
/// While the value is borrowed it is not available in TLS.
///
/// # Safety note
///
/// Does not validate the pointer type.
pub unsafe fn borrow<T>(f: |&mut T|) {
    let mut value = take();

    // XXX: Need a different abstraction from 'finally' here to avoid unsafety
    let unsafe_ptr = cast::transmute_mut_region(&mut *value);
    let value_cell = Cell::new(value);

    (|| f(unsafe_ptr)).finally(|| put(value_cell.take()));
}

/// Compiled implementation of accessing the runtime local pointer. This is
/// implemented using LLVM's thread_local attribute which isn't necessarily
/// working on all platforms. This implementation is faster, however, so we use
/// it wherever possible.
#[cfg(not(windows), not(target_os = "android"))]
pub mod compiled {
    #[cfg(not(test))]
    use libc::c_void;
    use cast;
    use option::{Option, Some, None};

    #[cfg(test)]
    pub use realstd::rt::shouldnt_be_public::RT_TLS_PTR;

    #[cfg(not(test))]
    #[thread_local]
    pub static mut RT_TLS_PTR: *mut c_void = 0 as *mut c_void;

    pub fn init() {}

    pub unsafe fn cleanup() {}

    /// Give a pointer to thread-local storage.
    ///
    /// # Safety note
    ///
    /// Does not validate the pointer type.
    #[inline]
    pub unsafe fn put<T>(sched: ~T) {
        RT_TLS_PTR = cast::transmute(sched)
    }

    /// Take ownership of a pointer from thread-local storage.
    ///
    /// # Safety note
    ///
    /// Does not validate the pointer type.
    #[inline]
    pub unsafe fn take<T>() -> ~T {
        let ptr: ~T = cast::transmute(RT_TLS_PTR);
        // can't use `as`, due to type not matching with `cfg(test)`
        RT_TLS_PTR = cast::transmute(0);
        ptr
    }

    /// Take ownership of a pointer from thread-local storage.
    ///
    /// # Safety note
    ///
    /// Does not validate the pointer type.
    /// Leaves the old pointer in TLS for speed.
    #[inline]
    pub unsafe fn unsafe_take<T>() -> ~T {
        cast::transmute(RT_TLS_PTR)
    }

    /// Check whether there is a thread-local pointer installed.
    pub fn exists() -> bool {
        unsafe {
            RT_TLS_PTR.is_not_null()
        }
    }

    pub unsafe fn unsafe_borrow<T>() -> *mut T {
        if RT_TLS_PTR.is_null() {
            rtabort!("thread-local pointer is null. bogus!");
        }
        RT_TLS_PTR as *mut T
    }

    pub unsafe fn try_unsafe_borrow<T>() -> Option<*mut T> {
        if RT_TLS_PTR.is_null() {
            None
        } else {
            Some(RT_TLS_PTR as *mut T)
        }
    }
}

/// Native implementation of having the runtime thread-local pointer. This
/// implementation uses the `thread_local_storage` module to provide a
/// thread-local value.
pub mod native {
    use cast;
    use libc::c_void;
    use option::{Option, Some, None};
    use ptr;
    use tls = rt::thread_local_storage;
    use unstable::mutex::{Mutex, MUTEX_INIT};

    static mut LOCK: Mutex = MUTEX_INIT;
    static mut INITIALIZED: bool = false;
    static mut RT_TLS_KEY: tls::Key = -1;

    /// Initialize the TLS key. Other ops will fail if this isn't executed
    /// first.
    pub fn init() {
        unsafe {
            LOCK.lock();
            if !INITIALIZED {
                tls::create(&mut RT_TLS_KEY);
                INITIALIZED = true;
            }
            LOCK.unlock();
        }
    }

    pub unsafe fn cleanup() {
        assert!(INITIALIZED);
        tls::destroy(RT_TLS_KEY);
        LOCK.destroy();
        INITIALIZED = false;
    }

    /// Give a pointer to thread-local storage.
    ///
    /// # Safety note
    ///
    /// Does not validate the pointer type.
    #[inline]
    pub unsafe fn put<T>(sched: ~T) {
        let key = tls_key();
        let void_ptr: *mut c_void = cast::transmute(sched);
        tls::set(key, void_ptr);
    }

    /// Take ownership of a pointer from thread-local storage.
    ///
    /// # Safety note
    ///
    /// Does not validate the pointer type.
    #[inline]
    pub unsafe fn take<T>() -> ~T {
        let key = tls_key();
        let void_ptr: *mut c_void = tls::get(key);
        if void_ptr.is_null() {
            rtabort!("thread-local pointer is null. bogus!");
        }
        let ptr: ~T = cast::transmute(void_ptr);
        tls::set(key, ptr::mut_null());
        return ptr;
    }

    /// Take ownership of a pointer from thread-local storage.
    ///
    /// # Safety note
    ///
    /// Does not validate the pointer type.
    /// Leaves the old pointer in TLS for speed.
    #[inline]
    pub unsafe fn unsafe_take<T>() -> ~T {
        let key = tls_key();
        let void_ptr: *mut c_void = tls::get(key);
        if void_ptr.is_null() {
            rtabort!("thread-local pointer is null. bogus!");
        }
        let ptr: ~T = cast::transmute(void_ptr);
        return ptr;
    }

    /// Check whether there is a thread-local pointer installed.
    pub fn exists() -> bool {
        unsafe {
            match maybe_tls_key() {
                Some(key) => tls::get(key).is_not_null(),
                None => false
            }
        }
    }

    /// Borrow a mutable reference to the thread-local value
    ///
    /// # Safety Note
    ///
    /// Because this leaves the value in thread-local storage it is possible
    /// For the Scheduler pointer to be aliased
    pub unsafe fn unsafe_borrow<T>() -> *mut T {
        let key = tls_key();
        let void_ptr = tls::get(key);
        if void_ptr.is_null() {
            rtabort!("thread-local pointer is null. bogus!");
        }
        void_ptr as *mut T
    }

    pub unsafe fn try_unsafe_borrow<T>() -> Option<*mut T> {
        match maybe_tls_key() {
            Some(key) => {
                let void_ptr = tls::get(key);
                if void_ptr.is_null() {
                    None
                } else {
                    Some(void_ptr as *mut T)
                }
            }
            None => None
        }
    }

    #[inline]
    fn tls_key() -> tls::Key {
        match maybe_tls_key() {
            Some(key) => key,
            None => rtabort!("runtime tls key not initialized")
        }
    }

    #[inline]
    #[cfg(not(test))]
    pub fn maybe_tls_key() -> Option<tls::Key> {
        unsafe {
            // NB: This is a little racy because, while the key is
            // initalized under a mutex and it's assumed to be initalized
            // in the Scheduler ctor by any thread that needs to use it,
            // we are not accessing the key under a mutex.  Threads that
            // are not using the new Scheduler but still *want to check*
            // whether they are running under a new Scheduler may see a 0
            // value here that is in the process of being initialized in
            // another thread. I think this is fine since the only action
            // they could take if it was initialized would be to check the
            // thread-local value and see that it's not set.
            if RT_TLS_KEY != -1 {
                return Some(RT_TLS_KEY);
            } else {
                return None;
            }
        }
    }

    #[inline] #[cfg(test)]
    pub fn maybe_tls_key() -> Option<tls::Key> {
        use realstd;
        unsafe {
            cast::transmute(realstd::rt::shouldnt_be_public::maybe_tls_key())
        }
    }
}
