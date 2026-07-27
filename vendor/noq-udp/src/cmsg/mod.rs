use std::{
    ffi::{c_int, c_uchar},
    ptr,
};

#[cfg(unix)]
#[path = "unix.rs"]
mod imp;

#[cfg(windows)]
#[path = "windows.rs"]
mod imp;

pub(crate) use imp::Aligned;

/// Helper to encode a series of control messages (native "cmsgs") to a buffer for use in `sendmsg`
//  like API.
///
/// The operation must be "finished" for the native msghdr to be usable, either by calling `finish`
/// explicitly or by dropping the `Encoder`.
pub(crate) struct Encoder<'a, M: MsgHdr> {
    hdr: &'a mut M,
    cmsg: Option<&'a mut M::ControlMessage>,
    len: usize,
}

impl<'a, M: MsgHdr> Encoder<'a, M> {
    /// # Safety
    /// - `hdr` must contain a suitably aligned pointer to a big enough buffer to hold control messages
    ///   bytes. All bytes of this buffer can be safely written.
    /// - The `Encoder` must be dropped before `hdr` is passed to a system call, and must not be leaked.
    pub(crate) unsafe fn new(hdr: &'a mut M) -> Self {
        Self {
            cmsg: unsafe { hdr.cmsg_first_hdr().as_mut() },
            hdr,
            len: 0,
        }
    }

    /// Append a control message to the buffer.
    ///
    /// # Panics
    /// - If insufficient buffer space remains.
    pub(crate) fn push<T: Copy>(&mut self, level: c_int, ty: c_int, value: T) {
        let space = M::ControlMessage::cmsg_space(size_of_val(&value));
        assert!(
            self.hdr.control_len() >= self.len + space,
            "control message buffer too small. Required: {}, Available: {}",
            self.len + space,
            self.hdr.control_len()
        );
        let cmsg = self.cmsg.take().expect("no control buffer space remaining");
        cmsg.set(level, ty, M::ControlMessage::cmsg_len(size_of_val(&value)));
        // Unaligned: CMSG_DATA is only guaranteed aligned for the native
        // cmsghdr, whose alignment is libc-dependent (musl: 4, glibc: 8) and
        // may be weaker than T's.
        unsafe {
            ptr::write_unaligned(cmsg.cmsg_data() as *const T as *mut T, value);
        }
        self.len += space;
        self.cmsg = unsafe { self.hdr.cmsg_nxt_hdr(cmsg).as_mut() };
    }

    /// Finishes appending control messages to the buffer
    pub(crate) fn finish(self) {
        // Delegates to the `Drop` impl
    }
}

// Statically guarantees that the encoding operation is "finished" before the control buffer is read
// by `sendmsg` like API.
impl<M: MsgHdr> Drop for Encoder<'_, M> {
    fn drop(&mut self) {
        self.hdr.set_control_len(self.len as _);
    }
}

/// # Safety
///
/// `cmsg` must refer to a native cmsg containing a payload of type `T`
pub(crate) unsafe fn decode<T: Copy, C: CMsgHdr>(cmsg: &impl CMsgHdr) -> T {
    debug_assert_eq!(cmsg.len(), C::cmsg_len(size_of::<T>()));
    // Unaligned: the kernel lays out cmsg payloads at sizeof(long), but Rust
    // only knows align_of::<cmsghdr>(), which musl declares as 4 — an aligned
    // read of a payload such as timespec (align 8) is not justifiable there.
    // (The former `assert!(align_of::<T>() <= align_of::<C>())` aborted every
    // musl build on its first received datagram.)
    unsafe { ptr::read_unaligned(cmsg.cmsg_data() as *const T) }
}

pub(crate) struct Iter<'a, M: MsgHdr> {
    hdr: &'a M,
    cmsg: Option<&'a M::ControlMessage>,
}

impl<'a, M: MsgHdr> Iter<'a, M> {
    /// # Safety
    ///
    /// `hdr` must hold a pointer to memory outliving `'a` which can be soundly read for the
    /// lifetime of the constructed `Iter` and contains a buffer of native cmsgs, i.e. is aligned
    //  for native `cmsghdr`, is fully initialized, and has correct internal links.
    pub(crate) unsafe fn new(hdr: &'a M) -> Self {
        Self {
            hdr,
            cmsg: unsafe { hdr.cmsg_first_hdr().as_ref() },
        }
    }
}

impl<'a, M: MsgHdr> Iterator for Iter<'a, M> {
    type Item = &'a M::ControlMessage;

    fn next(&mut self) -> Option<Self::Item> {
        let current = self.cmsg.take()?;
        self.cmsg = unsafe { self.hdr.cmsg_nxt_hdr(current).as_ref() };

        #[cfg(apple_fast)]
        {
            // On MacOS < 14 CMSG_NXTHDR might continuously return a zeroed cmsg. In
            // such case, return `None` instead, thus indicating the end of
            // the cmsghdr chain.
            if current.len() < size_of::<M::ControlMessage>() {
                return None;
            }
        }

        Some(current)
    }
}

// Helper traits for native types for control messages
pub(crate) trait MsgHdr {
    type ControlMessage: CMsgHdr;

    fn cmsg_first_hdr(&self) -> *mut Self::ControlMessage;

    fn cmsg_nxt_hdr(&self, cmsg: &Self::ControlMessage) -> *mut Self::ControlMessage;

    /// Sets the number of control messages added to this `struct msghdr`.
    ///
    /// Note that this is a destructive operation and should only be done as a finalisation
    /// step.
    fn set_control_len(&mut self, len: usize);

    fn control_len(&self) -> usize;
}

pub(crate) trait CMsgHdr {
    fn cmsg_len(length: usize) -> usize;

    fn cmsg_space(length: usize) -> usize;

    fn cmsg_data(&self) -> *mut c_uchar;

    fn set(&mut self, level: c_int, ty: c_int, len: usize);

    fn len(&self) -> usize;
}

#[cfg(unix)]
pub(crate) const LEN: usize = 96;

#[cfg(test)]
mod tests {
    use super::*;

    /// musl's `struct cmsghdr` on 64-bit little-endian: four ints — 16 bytes,
    /// align 4. glibc's starts with a `size_t`, align 8. The regression this
    /// pins down: a payload whose alignment (timespec: 8) exceeds the header
    /// type's, which `decode`/`push` used to reject with an align_of assert —
    /// aborting every musl build on its first received datagram — and now
    /// must handle with unaligned reads/writes.
    #[repr(C)]
    #[derive(Copy, Clone)]
    struct MuslCmsghdr {
        cmsg_len: u32,
        _pad1: i32,
        cmsg_level: i32,
        cmsg_type: i32,
    }

    const _: () = assert!(align_of::<MuslCmsghdr>() == 4);

    impl CMsgHdr for MuslCmsghdr {
        fn cmsg_len(length: usize) -> usize {
            size_of::<Self>() + length
        }

        fn cmsg_space(length: usize) -> usize {
            size_of::<Self>() + ((length + 7) & !7)
        }

        fn cmsg_data(&self) -> *mut c_uchar {
            unsafe { (self as *const Self).add(1) as *mut c_uchar }
        }

        fn set(&mut self, level: c_int, ty: c_int, len: usize) {
            self.cmsg_level = level;
            self.cmsg_type = ty;
            self.cmsg_len = len as u32;
        }

        fn len(&self) -> usize {
            self.cmsg_len as usize
        }
    }

    /// A single-message control buffer is all these tests need.
    struct MuslMsghdr {
        control: *mut c_uchar,
        controllen: usize,
    }

    impl MsgHdr for MuslMsghdr {
        type ControlMessage = MuslCmsghdr;

        fn cmsg_first_hdr(&self) -> *mut MuslCmsghdr {
            if self.controllen >= size_of::<MuslCmsghdr>() {
                self.control as *mut MuslCmsghdr
            } else {
                ptr::null_mut()
            }
        }

        fn cmsg_nxt_hdr(&self, _cmsg: &MuslCmsghdr) -> *mut MuslCmsghdr {
            ptr::null_mut()
        }

        fn set_control_len(&mut self, len: usize) {
            self.controllen = len;
        }

        fn control_len(&self) -> usize {
            self.controllen
        }
    }

    #[repr(C)]
    #[derive(Copy, Clone, PartialEq, Debug)]
    struct Timespec64 {
        tv_sec: i64,
        tv_nsec: i64,
    }

    const _: () = assert!(align_of::<Timespec64>() == 8);

    /// An 8-aligned backing buffer with the header placed at +4: valid for
    /// `MuslCmsghdr` (align 4), and CMSG_DATA (header + 16) lands at +20 —
    /// genuinely misaligned for the payload. Stricter than the real musl
    /// layout, where the kernel happens to 8-align the data and only the
    /// align_of assert fired: passing here proves the unaligned access is
    /// sufficient even when the payload truly is misaligned.
    #[repr(align(8))]
    struct Buf([u8; 64]);

    const TS: Timespec64 = Timespec64 {
        tv_sec: 0x0102_0304_0506_0708,
        tv_nsec: 0x1112_1314_1516_1718,
    };

    #[test]
    fn decode_reads_payloads_more_aligned_than_the_header_type() {
        let mut buf = Buf([0; 64]);
        unsafe {
            let hdr = buf.0.as_mut_ptr().add(4) as *mut MuslCmsghdr;
            (*hdr).set(1, 2, MuslCmsghdr::cmsg_len(size_of::<Timespec64>()));
            assert_eq!((*hdr).cmsg_data() as usize % align_of::<Timespec64>(), 4);
            ptr::write_unaligned((*hdr).cmsg_data() as *mut Timespec64, TS);
            let got: Timespec64 = decode::<Timespec64, MuslCmsghdr>(&*hdr);
            assert_eq!(got, TS);
        }
    }

    #[test]
    fn push_writes_payloads_more_aligned_than_the_header_type() {
        let mut buf = Buf([0; 64]);
        let mut hdr = MuslMsghdr {
            control: unsafe { buf.0.as_mut_ptr().add(4) },
            controllen: 48,
        };
        let mut encoder = unsafe { Encoder::new(&mut hdr) };
        encoder.push(1, 2, TS);
        encoder.finish();
        assert_eq!(hdr.control_len(), MuslCmsghdr::cmsg_space(size_of::<Timespec64>()));
        unsafe {
            let cmsg = &*hdr.cmsg_first_hdr();
            assert_eq!(cmsg.cmsg_data() as usize % align_of::<Timespec64>(), 4);
            let got: Timespec64 = ptr::read_unaligned(cmsg.cmsg_data() as *const Timespec64);
            assert_eq!(got, TS);
        }
    }
}
