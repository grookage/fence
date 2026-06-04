//! Platform-specific cache flush and memory fence operations.
//!
//! On x86-64 (production), uses `clwb` + `sfence` for cache writeback to the
//! persistence domain (ADR-covered CXL memory).
//!
//! On other architectures (macOS ARM dev), provides no-op stubs. Protocol
//! correctness is still testable via atomic ordering; persistence guarantees
//! require x86-64 + ADR hardware.

/// Flush a single cacheline containing `addr` to the persistence domain.
///
/// On x86-64: executes `CLWB` (cache line write back). The line remains
/// in cache (unlike `CLFLUSH` which evicts it), so subsequent accesses
/// are still fast.
///
/// On non-x86-64: no-op.
///
/// # Safety
///
/// `addr` must point to a valid, mapped memory location. The caller must
/// ensure the address is within a live mmap'd region.
#[inline(always)]
pub unsafe fn clwb(addr: *const u8) {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: Caller guarantees addr is within a valid mapping.
        core::arch::x86_64::_mm_clwb(addr as *const u8);
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = addr;
        // No-op on non-x86 platforms (ARM macOS dev).
    }
}

/// Store fence — orders all prior stores before subsequent stores.
///
/// On x86-64: executes `SFENCE`. Ensures that all `CLWB` instructions
/// issued before this fence are globally visible before any stores after it.
///
/// On non-x86-64: compiler fence only (prevents compiler reordering).
///
/// # Safety
///
/// No memory safety requirements — this is a CPU ordering instruction.
/// Marked unsafe to match the pattern of persistence primitives and to
/// signal "you should know what you're doing with fence ordering."
#[inline(always)]
pub unsafe fn sfence() {
    #[cfg(target_arch = "x86_64")]
    {
        core::arch::x86_64::_mm_sfence();
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        // Compiler fence prevents the compiler from reordering stores
        // across this point. Not a hardware fence, but sufficient for
        // development correctness testing on ARM.
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    }
}

/// Flush all cachelines covering the byte range `[addr, addr + len)` to
/// the persistence domain, then issue a store fence.
///
/// This is the standard "persist a region" operation:
/// 1. CLWB every 64-byte aligned cacheline in the range.
/// 2. SFENCE to ensure all flushes complete before subsequent stores.
///
/// # Safety
///
/// Caller must ensure:
/// - `addr` points to a valid, mapped memory region.
/// - `addr + len` does not exceed the mapping bounds.
/// - The region remains mapped for the duration of this call.
#[inline]
pub unsafe fn flush_range(addr: *const u8, len: usize) {
    if len == 0 {
        return;
    }

    // Align start down to cacheline boundary
    let start = (addr as usize) & !(super::layout::CACHELINE - 1);
    let end = (addr as usize) + len;

    let mut ptr = start;
    while ptr < end {
        // SAFETY: ptr is within [aligned_start, addr+len), all within the mapping.
        clwb(ptr as *const u8);
        ptr += super::layout::CACHELINE;
    }

    // SAFETY: No memory safety requirements for sfence.
    sfence();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flush_range_no_panic_on_stack_array() {
        let data = [0u8; 256];
        // Just ensure it doesn't panic or crash.
        // On ARM this is a no-op; on x86-64 it exercises real CLWB.
        unsafe {
            flush_range(data.as_ptr(), data.len());
        }
    }

    #[test]
    fn flush_range_zero_len_no_panic() {
        let data = [0u8; 64];
        unsafe {
            flush_range(data.as_ptr(), 0);
        }
    }

    #[test]
    fn flush_range_unaligned_start() {
        let data = [0u8; 200];
        // Start at an unaligned offset within the array
        unsafe {
            flush_range(data.as_ptr().add(7), 100);
        }
    }

    #[test]
    fn clwb_single_line() {
        let data = [0u8; 64];
        unsafe {
            clwb(data.as_ptr());
        }
    }

    #[test]
    fn sfence_standalone() {
        unsafe {
            sfence();
        }
    }
}
