//! Counting global allocator used to verify the steady-state frame path does
//! not allocate (spec 4). Install with:
//!
//! ```ignore
//! #[global_allocator]
//! static ALLOC: jellyroll_platform::alloc_counter::CountingAlloc = jellyroll_platform::alloc_counter::CountingAlloc;
//! ```
//!
//! Counts are per thread, so the render thread's frames are measured
//! without the network and image-decoding threads' work (which allocates
//! freely, off the frame path). Cost is one thread-local increment per
//! allocation.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    // Const-initialized and without a destructor: accessing it never
    // allocates, so it is safe to use from inside the allocator.
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}

fn count() {
    // `try_with`: the slot may be gone while a thread exits.
    let _ = ALLOCATIONS.try_with(|c| c.set(c.get() + 1));
}

pub struct CountingAlloc;

// SAFETY: forwards every call unchanged to the system allocator.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        // SAFETY: caller upholds GlobalAlloc's contract.
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count();
        // SAFETY: caller upholds GlobalAlloc's contract.
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count();
        // SAFETY: caller upholds GlobalAlloc's contract.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: caller upholds GlobalAlloc's contract.
        unsafe { System.dealloc(ptr, layout) }
    }
}

/// Allocations (including reallocs) made by the calling thread since it
/// started.
pub fn allocations() -> u64 {
    ALLOCATIONS.try_with(Cell::get).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn counts_only_this_thread() {
        // The test harness may not install the counting allocator; just
        // check other threads' counts never show up here.
        let before = super::allocations();
        std::thread::spawn(|| drop(vec![0u8; 64])).join().unwrap();
        assert_eq!(super::allocations(), before);
    }
}
