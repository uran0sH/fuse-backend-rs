// Copyright (C) 2021 Alibaba Cloud. All rights reserved.
//
// SPDX-License-Identifier: Apache-2.0 OR BSD-3-Clause

//! Helper structures to work around limitations of the `vm-memory` crate.
//!
//! The vm-memory v0.6.0 introduced support of dirty page tracking by using `Bitmap`, which adds a
//! generic type parameters to several APIs. That's a breaking change and  makes the rust compiler
//! fail to compile our code. So introduce [FileVolatileSlice] to mask out the `BitmapSlice`
//! generic type parameter.
//!
//! Dirty page tracking is handled at higher level in `IoBuffers`.

use std::io::{Read, Write};
use std::marker::PhantomData;
use std::sync::atomic::Ordering;
use std::{error, fmt};

use vm_memory::{
    bitmap::BitmapSlice, volatile_memory::Error as VError, AtomicAccess, Bytes, VolatileSlice,
};

/// [`FileVolatileSlice`] related errors.
#[allow(missing_docs)]
#[derive(Debug)]
pub enum Error {
    /// `addr` is out of bounds of the volatile memory slice.
    OutOfBounds { addr: usize },
    /// Taking a slice at `base` with `offset` would overflow `usize`.
    Overflow { base: usize, offset: usize },
    /// The error of VolatileSlice.
    VolatileSlice(VError),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::OutOfBounds { addr } => write!(f, "address 0x{:x} is out of bounds", addr),
            Error::Overflow { base, offset } => write!(
                f,
                "address 0x{:x} offset by 0x{:x} would overflow",
                base, offset
            ),
            Error::VolatileSlice(e) => write!(f, "{}", e),
        }
    }
}

impl error::Error for Error {}

/// An adapter structure to work around limitations of the `vm-memory` crate.
///
/// It solves the compilation failure by masking out the
/// [`vm_memory::BitmapSlice`](https://docs.rs/vm-memory/latest/vm_memory/bitmap/trait.BitmapSlice.html)
/// generic type parameter of
/// [`vm_memory::VolatileSlice`](https://docs.rs/vm-memory/latest/vm_memory/volatile_memory/struct.VolatileSlice.html)
#[derive(Clone, Copy, Debug)]
pub struct FileVolatileSlice<'a> {
    addr: usize,
    size: usize,
    phantom: PhantomData<&'a u8>,
}

impl<'a> FileVolatileSlice<'a> {
    /// Create a new instance of [`FileVolatileSlice`] from a raw pointer.
    ///
    /// # Safety
    /// To use this safely, the caller must guarantee that the memory at `addr` is `size` bytes long
    /// and is available for the duration of the lifetime of the new [FileVolatileSlice].
    /// The caller must also guarantee that all other users of the given chunk of memory are using
    /// volatile accesses.
    ///
    /// ### Example
    /// ```rust
    /// # use fuse_backend_rs::transport::FileVolatileSlice;
    /// # use vm_memory::bytes::Bytes;
    /// # use std::sync::atomic::Ordering;
    /// let mut buffer = [0u8; 1024];
    /// let s = unsafe { FileVolatileSlice::new(buffer.as_mut_ptr(), buffer.len()) };
    ///
    /// {
    ///     let o: u32 = s.load(0x10, Ordering::Acquire).unwrap();
    ///     assert_eq!(o, 0);
    ///     s.store(1u8, 0x10, Ordering::Release).unwrap();
    ///
    ///     let s2 = s.as_volatile_slice();
    ///     let s3 = FileVolatileSlice::new_from_volatile_slice(&s2);
    ///     assert_eq!(s3.len(), 1024);
    /// }
    ///
    /// assert_eq!(buffer[0x10], 1);
    /// ```
    pub unsafe fn new(addr: *mut u8, size: usize) -> Self {
        Self {
            addr: addr as usize,
            size,
            phantom: PhantomData,
        }
    }

    /// Create a new [`FileVolatileSlice`] from [`VolatileSlice`](https://docs.rs/vm-memory/latest/vm_memory/volatile_memory/struct.VolatileSlice.html)
    /// and strip off the [`BitmapSlice`](https://docs.rs/vm-memory/latest/vm_memory/bitmap/trait.BitmapSlice.html) generic type parameter.
    ///
    /// The caller needs to handle dirty page tracking for the data buffer.
    pub fn new_from_volatile_slice<S: BitmapSlice>(s: &VolatileSlice<'a, S>) -> Self {
        unsafe { Self::new(s.as_ptr(), s.len()) }
    }

    /// Create a [`vm_memory::VolatileSlice`](https://docs.rs/vm-memory/latest/vm_memory/volatile_memory/struct.VolatileSlice.html)
    /// from [FileVolatileSlice] without dirty page tracking.
    pub fn as_volatile_slice(&self) -> VolatileSlice<'a, ()> {
        unsafe { VolatileSlice::new(self.as_ptr(), self.len()) }
    }

    /// Return a pointer to the start of the slice.
    pub fn as_ptr(&self) -> *mut u8 {
        self.addr as *mut u8
    }

    /// Get the size of the slice.
    pub fn len(&self) -> usize {
        self.size
    }

    /// Check if the slice is empty.
    pub fn is_empty(&self) -> bool {
        self.size == 0
    }

    /// Return a subslice of this [FileVolatileSlice] starting at `offset`.
    pub fn offset(&self, count: usize) -> Result<Self, Error> {
        let new_addr = (self.addr as usize)
            .checked_add(count)
            .ok_or(Error::Overflow {
                base: self.addr as usize,
                offset: count,
            })?;
        let new_size = self
            .size
            .checked_sub(count)
            .ok_or(Error::OutOfBounds { addr: new_addr })?;
        unsafe { Ok(Self::new(new_addr as *mut u8, new_size)) }
    }
}

impl<'a> Bytes<usize> for FileVolatileSlice<'a> {
    type E = VError;

    fn write(&self, buf: &[u8], addr: usize) -> Result<usize, Self::E> {
        VolatileSlice::write(&self.as_volatile_slice(), buf, addr)
    }

    fn read(&self, buf: &mut [u8], addr: usize) -> Result<usize, Self::E> {
        VolatileSlice::read(&self.as_volatile_slice(), buf, addr)
    }

    fn write_slice(&self, buf: &[u8], addr: usize) -> Result<(), Self::E> {
        VolatileSlice::write_slice(&self.as_volatile_slice(), buf, addr)
    }

    fn read_slice(&self, buf: &mut [u8], addr: usize) -> Result<(), Self::E> {
        VolatileSlice::write_slice(&self.as_volatile_slice(), buf, addr)
    }

    fn read_from<F>(&self, addr: usize, src: &mut F, count: usize) -> Result<usize, Self::E>
    where
        F: Read,
    {
        VolatileSlice::read_from(&self.as_volatile_slice(), addr, src, count)
    }

    fn read_exact_from<F>(&self, addr: usize, src: &mut F, count: usize) -> Result<(), Self::E>
    where
        F: Read,
    {
        VolatileSlice::read_exact_from(&self.as_volatile_slice(), addr, src, count)
    }

    fn write_to<F>(&self, addr: usize, dst: &mut F, count: usize) -> Result<usize, Self::E>
    where
        F: Write,
    {
        VolatileSlice::write_to(&self.as_volatile_slice(), addr, dst, count)
    }

    fn write_all_to<F>(&self, addr: usize, dst: &mut F, count: usize) -> Result<(), Self::E>
    where
        F: Write,
    {
        VolatileSlice::write_all_to(&self.as_volatile_slice(), addr, dst, count)
    }

    fn store<T: AtomicAccess>(&self, val: T, addr: usize, order: Ordering) -> Result<(), Self::E> {
        VolatileSlice::store(&self.as_volatile_slice(), val, addr, order)
    }

    fn load<T: AtomicAccess>(&self, addr: usize, order: Ordering) -> Result<T, Self::E> {
        VolatileSlice::load(&self.as_volatile_slice(), addr, order)
    }
}

#[cfg(feature = "async-io")]
pub use async_io::FileVolatileBuf;

#[cfg(feature = "async-io")]
mod async_io {
    use super::*;
    use tokio_uring::buf::{IoBuf, IoBufMut};

    /// An adapter structure to support `io-uring` based asynchronous IO.
    ///
    /// The `tokio-uring` framework needs to take ownership of data buffers during asynchronous IO
    /// operations. The [FileVolatileBuf] converts a referenced buffer to a buffer compatible with
    /// the `tokio-uring` APIs.
    ///
    /// # Safety
    /// The buffer is borrowed without a lifetime parameter, so the caller must ensure that
    /// the [FileVolatileBuf] object doesn't out-live the borrowed buffer. And during the lifetime
    /// of the [FileVolatileBuf] object, the referenced buffer must be stable.
    #[derive(Clone, Copy, Debug)]
    pub struct FileVolatileBuf {
        addr: usize,
        size: usize,
        cap: usize,
    }

    impl FileVolatileBuf {
        /// Create a [FileVolatileBuf] object from a buffer.
        pub unsafe fn new(buf: &mut [u8]) -> Self {
            Self {
                addr: buf.as_mut_ptr() as usize,
                size: 0,
                cap: buf.len(),
            }
        }

        /// Create a [FileVolatileBuf] object from a raw pointer.
        pub unsafe fn from_raw(addr: *mut u8, size: usize, cap: usize) -> Self {
            Self {
                addr: addr as usize,
                size,
                cap,
            }
        }
    }

    unsafe impl IoBuf for FileVolatileBuf {
        fn stable_ptr(&self) -> *const u8 {
            self.addr as *const u8
        }

        fn bytes_init(&self) -> usize {
            self.size
        }

        fn bytes_total(&self) -> usize {
            self.cap
        }
    }

    unsafe impl IoBufMut for FileVolatileBuf {
        fn stable_mut_ptr(&mut self) -> *mut u8 {
            self.addr as *mut u8
        }

        unsafe fn set_init(&mut self, pos: usize) {
            self.size = pos;
        }
    }

    impl<'a> FileVolatileSlice<'a> {
        /// Borrow a [FileVolatileSlice] to temporarily elide the lifetime parameter.
        ///
        /// # Safety
        /// The [FileVolatileSlice] is borrowed without a lifetime parameter, so the caller must
        /// ensure that [FileVolatileBuf] doesn't out-live the borrowed [FileVolatileSlice] object.
        pub unsafe fn borrow_mut(&self) -> FileVolatileBuf {
            FileVolatileBuf {
                addr: self.addr,
                size: 0,
                cap: self.size,
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_new_file_volatile_buf() {
            let mut buf = [0u8; 1024];
            let mut buf2 = unsafe { FileVolatileBuf::new(&mut buf) };
            assert_eq!(buf2.bytes_total(), 1024);
            assert_eq!(buf2.bytes_init(), 0);
            assert_eq!(buf2.stable_ptr(), buf.as_ptr());
            unsafe { *buf2.stable_mut_ptr() = b'a' };
            assert_eq!(buf[0], b'a');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_file_volatile_slice() {
        let mut buffer = [0u8; 1024];
        let s = unsafe { FileVolatileSlice::new(buffer.as_mut_ptr(), buffer.len()) };

        let o: u32 = s.load(0x10, Ordering::Acquire).unwrap();
        assert_eq!(o, 0);
        s.store(1u8, 0x10, Ordering::Release).unwrap();

        let s2 = s.as_volatile_slice();
        let s3 = FileVolatileSlice::new_from_volatile_slice(&s2);
        assert_eq!(s3.len(), 1024);

        assert!(s3.offset(2048).is_err());

        assert_eq!(buffer[0x10], 1);
    }
}
