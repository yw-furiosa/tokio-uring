//! A dynamic collection of I/O buffers pre-registered with the kernel.
//!
//! This module provides [`FixedBufRegister`], a collection that implements
//! dynamic management of sets of interchangeable memory buffers
//! registered with the kernel for `io-uring` operations.
//!
//! [`FixedBufRegister`]: self::FixedBufRegister

use super::plumbing;

use crate::buf::BufferImpl;
use crate::runtime::CONTEXT;
use crate::Buffer;
use std::io;
use std::sync::{Arc, Mutex};

/// An indexed collection of I/O buffers pre-registered with the kernel.
///
/// `FixedBufRegistry` allows the application to manage a collection of buffers
/// allocated in memory, that can be registered in the current `tokio-uring`
/// context using the [`register`] function. The buffers are accessed by their
/// indices using the [`check_out`] method.
///
/// A `FixedBufRegistry` value is a lightweight handle for a collection of
/// allocated buffers. Cloning of a `FixedBufRegistry` creates a new reference to
/// the same collection of buffers.
///
/// The buffers of the collection are not deallocated until:
/// - all `FixedBufRegistry` references to the collection have been dropped;
/// - all ['Buffer']s check-outed from the collection have
///   been dropped, including the buffer handles owned by any I/O operations
///   in flight;
///
/// [`register`]: register
/// [`check_out`]: Self::check_out
/// [`Runtime`]: crate::Runtime
/// ['Buffer']: crate::Buffer
#[derive(Clone)]
pub struct FixedBufRegistry {
    inner: Arc<Mutex<plumbing::Registry>>,
}

impl FixedBufRegistry {
    fn new(inner: plumbing::Registry) -> Self {
        Self {
            inner: Arc::new(Mutex::new(inner)),
        }
    }

    /// Returns a buffer identified by the specified index for use by the
    /// application, unless the buffer is already in use.
    ///
    /// The buffer is released to be available again once the
    /// returned `Buffer` handle has been dropped. An I/O operation
    /// using the buffer takes ownership of it and returns it once completed,
    /// preventing shared use of the buffer while the operation is in flight.
    pub fn check_out(&self, index: usize) -> Option<Buffer> {
        let (iovec, init_len) = {
            let mut inner = self.inner.lock().unwrap();
            inner.check_out(index)?
        };

        let registry_info = RegistryInfo {
            registry: self.inner.clone(),
            index: index as u16,
        };
        let buf = FixedBuf {
            iovec,
            init_len,
            registry_info: Some(registry_info),
        };

        Some(Buffer::new(buf))
    }
}

/// Registers the buffers with the kernel and creates a new collection of buffers from the provided allocated vectors.
///
/// The returned collection takes up to [`UIO_MAXIOV`]
/// buffers from the input. Any items in excess of that amount are silently
/// dropped, unless the input iterator produces the vectors lazily.
///
/// [`UIO_MAXIOV`]: libc::UIO_MAXIOV
///
/// This method must be called in the context of a `tokio-uring` runtime.
/// The registration persists for the lifetime of the runtime, unless
/// revoked by the [`unregister`] method. Dropping the
/// `FixedBufRegistry` instance this method has been called on does not revoke
/// the registration or deallocate the buffers.
///
/// [`unregister`]: unregister
///
/// This call can be blocked in the kernel to complete any operations
/// in-flight on the same `io-uring` instance. The application is
/// recommended to register buffers before starting any I/O operations.
///
/// # Examples
///
/// When providing uninitialized vectors for the collection, take care to
/// not replicate a vector with `.clone()` as that does not preserve the
/// capacity and the resulting buffer pointer will be rejected by the kernel.
/// This means that the following use of [`iter::repeat`] would not work:
///
/// [`iter::repeat`]: std::iter::repeat
///
/// ```should_panic
/// use tokio_uring::buf::fixed::registry;
/// use tokio_uring::Buffer;
/// use std::iter;
///
/// # #[allow(non_snake_case)]
/// # fn main() -> Result<(), std::io::Error> {
/// # use nix::sys::resource::{getrlimit, Resource};
/// # let (memlock_limit, _) = getrlimit(Resource::RLIMIT_MEMLOCK)?;
/// # let NUM_BUFFERS = std::cmp::max(memlock_limit as usize / 4096 / 8, 1);
/// # let BUF_SIZE = 4096;
///
/// tokio_uring::start(async {
///     let registry = registry::register(iter::repeat(Vec::<u8>::with_capacity(BUF_SIZE)).take(NUM_BUFFERS).map(Buffer::from))?;
///     // ...
///     Ok(())
/// })
/// # }
/// ```
///
/// Instead, create the vectors with requested capacity directly:
///
/// ```
/// use tokio_uring::buf::fixed::registry;
/// use tokio_uring::Buffer;
/// use std::iter;
///
/// # #[allow(non_snake_case)]
/// # fn main() -> Result<(), std::io::Error> {
/// # use nix::sys::resource::{getrlimit, Resource};
/// # let (memlock_limit, _) = getrlimit(Resource::RLIMIT_MEMLOCK)?;
/// # let NUM_BUFFERS = std::cmp::max(memlock_limit as usize / 4096 / 8, 1);
/// # let BUF_SIZE = 4096;
///
/// tokio_uring::start(async {
///     let registry = registry::register(iter::repeat_with(|| Vec::<u8>::with_capacity(BUF_SIZE)).take(NUM_BUFFERS).map(Buffer::from))?;
///     // ...
///     Ok(())
/// })
/// # }
/// ```
///
/// # Errors
///
/// If a collection of buffers is currently registered in the context
/// of the `tokio-uring` runtime this call is made in, the function returns
/// an error.
pub fn register(bufs: impl Iterator<Item = Buffer>) -> io::Result<FixedBufRegistry> {
    let registry_inner = plumbing::Registry::new(bufs);
    CONTEXT.with(|x| {
        x.handle()
            .as_ref()
            .expect("Not in a runtime context")
            .register_buffers(registry_inner.iovecs())
    })?;
    Ok(FixedBufRegistry::new(registry_inner))
}

/// Unregisters this collection of buffers.
///
/// This method must be called in the context of a `tokio-uring` runtime,
/// where the buffers should have been previously registered.
///
/// This operation invalidates any `Buffer` handles checked out from
/// this registry instance. Continued use of such handles in I/O
/// operations may result in an error.
///
/// # Errors
///
/// Calling `unregister` when no `FixedBufRegistry` is currently
/// registered on this runtime returns an error. Any of 'Buffer' check-outed from 'FixedBufRegistry' is in used, also returns an error
pub fn unregister() -> io::Result<()> {
    CONTEXT.with(|x| {
        x.handle()
            .as_ref()
            .expect("Not in a runtime context")
            .unregister_buffers()
    })
}

pub(crate) struct FixedBuf {
    iovec: libc::iovec,
    init_len: usize,
    registry_info: Option<RegistryInfo>,
}

#[derive(Clone)]
pub(crate) struct RegistryInfo {
    registry: Arc<Mutex<plumbing::Registry>>,
    pub index: u16,
}

unsafe impl BufferImpl for FixedBuf {
    type UserData = RegistryInfo;

    fn into_raw_parts(mut self) -> (Vec<*mut u8>, Vec<usize>, Vec<usize>, Self::UserData) {
        let registry_info = self.registry_info.take().unwrap();
        (
            vec![self.iovec.iov_base as _],
            vec![self.init_len],
            vec![self.iovec.iov_len],
            registry_info,
        )
    }

    unsafe fn from_raw_parts(
        ptr: Vec<*mut u8>,
        len: Vec<usize>,
        cap: Vec<usize>,
        user_data: Self::UserData,
    ) -> Self {
        let iovec = libc::iovec {
            iov_base: ptr[0] as _,
            iov_len: cap[0],
        };
        FixedBuf {
            iovec,
            init_len: len[0],
            registry_info: Some(user_data),
        }
    }
}

impl Drop for FixedBuf {
    fn drop(&mut self) {
        let Some(registry_info) = self.registry_info.take() else {
            return;
        };
        let mut registry = registry_info.registry.lock().unwrap();
        registry.check_in(registry_info.index as usize, self.init_len);
    }
}
