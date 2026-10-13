//! Counting global allocator used to verify the steady-state frame path does
//! not allocate (spec 4). Install with:
//!
//! ```ignore
//! #[global_allocator]
//! static ALLOC: jellyroll_platform::alloc_counter::CountingAlloc = jellyroll_platform::alloc_counter::CountingAlloc;
//! ```
//!
//! Cost is one relaxed atomic increment per allocation.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);

pub struct CountingAlloc;

// SAFETY: forwards every call unchanged to the system allocator.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: caller upholds GlobalAlloc's contract.
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: caller upholds GlobalAlloc's contract.
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: caller upholds GlobalAlloc's contract.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: caller upholds GlobalAlloc's contract.
        unsafe { System.dealloc(ptr, layout) }
    }
}

/// Total allocations (including reallocs) process-wide since start. Counts
/// every thread; the render thread is the only busy one in M1.
pub fn allocations() -> u64 {
    ALLOCATIONS.load(Ordering::Relaxed)
}
