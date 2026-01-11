use alloc::alloc::handle_alloc_error;
use core::{
    alloc::{GlobalAlloc, Layout},
    ptr::NonNull,
};
use customizable_buddy::{BuddyAllocator, LinkedListBuddy, UsizeBuddy};

/// 初始化全局分配器和内核堆分配器。
pub fn init() {
    // 托管空间 16 KiB
    const MEMORY_SIZE: usize = 16 << 10;
    static mut MEMORY: [u8; MEMORY_SIZE] = [0u8; MEMORY_SIZE];
    unsafe {
        HEAP.init(
            core::mem::size_of::<usize>().trailing_zeros() as _,
            NonNull::new(MEMORY.as_mut_ptr()).unwrap(),
        );
        HEAP.transfer(NonNull::new_unchecked(MEMORY.as_mut_ptr()), MEMORY.len());
    }
}

type MutAllocator<const N: usize> = BuddyAllocator<N, UsizeBuddy, LinkedListBuddy>;

// RV64: 使用 32 层 buddy allocator (64-bit usize 支持更大的层级)
#[cfg(target_pointer_width = "64")]
static mut HEAP: MutAllocator<32> = MutAllocator::new();

// RV32: 使用 20 层 buddy allocator (32-bit usize 会在层级过高时导致位移溢出)
#[cfg(target_pointer_width = "32")]
static mut HEAP: MutAllocator<20> = MutAllocator::new();

struct Global;

#[global_allocator]
static GLOBAL: Global = Global;

unsafe impl GlobalAlloc for Global {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if let Ok((ptr, _)) = HEAP.allocate_layout::<u8>(layout) {
            ptr.as_ptr()
        } else {
            handle_alloc_error(layout)
        }
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        HEAP.deallocate_layout(NonNull::new(ptr).unwrap(), layout)
    }
}
