//! Heap-allocated GC handles.
//!
//! Each `Handle<T>` is a double indirection, similar to `*mut *mut T`.
//! The first pointer is to the slot in the handle block, and the second
//! to the actual value on the GC heap.
//!
//! The concept is similar to a shadow stack. We track which objects on the
//! heap are referenced from the stack by holding the actual references to
//! objects in a heap-allocated block. This heap-allocated block is
//! treated as part of the root set by the GC, so the referenced objects are
//! always considered live.
//!
//! That means having a `Handle<T>` allows derefencing to the `T`, without
//! risking a potential use-after-free, which may arise when an object
//! reference is held on the stack across GC points (such as an allocation):
//!
//! ```rust,ignore
//! let a = allocate();
//! let b = allocate();
//! ```
//!
//! In the above snippet, `a` may be dangling after the 2nd call to `allocate` if:
//! - The GC is not aware of `a`,
//! - `allocate` may trigger a GC cycle
//!
//! A handle solves the problem by ensuring that the GC is always aware of `a`.
//!
//! A nice side-effect of this scheme is that it enables using any precise garbage
//! collection algorithm under the hood.

use crate::alloc::Allocator;
use crate::alloc::Data;
use crate::alloc::GcCell;
use crate::gc::Trace;
use std::marker::PhantomData;
use std::ops::Deref;
use std::ptr::addr_of_mut;
use std::ptr::null_mut;

// Surely pages are at least 4kB!
const BLOCK_SIZE: usize = 4096 / std::mem::size_of::<Opaque>();

type Opaque = *mut GcCell<Data>;

type BlockList = Vec<Box<[Opaque; BLOCK_SIZE]>>;

pub trait Object: Sized + 'static {}

pub(crate) struct HandleScopeData {
    /// The next available handle slot.
    ///
    /// None left if `next == limit`.
    pub(crate) next: *mut Opaque,

    /// End of the handle block.
    limit: *mut Opaque,

    /// Handle scope nesting depth.
    level: usize,

    /// Handle block list, the last used block is the one pointed to by `scope_data`.
    pub(crate) blocks: BlockList,
}

impl HandleScopeData {
    pub(crate) fn new() -> Self {
        let mut this = Self {
            next: null_mut(),
            limit: null_mut(),
            level: 0,
            blocks: BlockList::new(),
        };
        unsafe {
            HandleScopeData::allocate_block(&mut this);
        }
        this
    }
}

impl HandleScopeData {
    #[cold]
    #[inline(never)]
    unsafe fn allocate_block(this: *mut HandleScopeData) {
        // Push a new block onto the list
        let blocks = addr_of_mut!((*this).blocks);
        (*blocks).push(Box::new([null_mut(); BLOCK_SIZE]));

        // Pointer to start of the new block
        let next = (*blocks).last_mut().unwrap_unchecked().as_mut_ptr();
        let limit = next.add(BLOCK_SIZE); // block~[BLOCK_SIZE + 1]

        debug!("next={next:p}, limit={limit:p}");

        (*this).next = next;
        (*this).limit = limit;
    }

    #[cold]
    #[inline(never)]
    unsafe fn free_unused_blocks(this: *mut HandleScopeData) {
        #[inline(always)]
        unsafe fn block_limit(blocks: *mut BlockList, index: usize) -> *mut Opaque {
            let slice = Box::into_raw((*blocks).as_mut_ptr().add(index).read());
            let start = slice as *mut Opaque;
            start.add(BLOCK_SIZE)
        }

        #[inline(always)]
        unsafe fn manually_drop_block_at(blocks: *mut BlockList, index: usize) {
            let block_box = (*blocks).as_mut_ptr().add(index);
            drop(block_box.read());
        }

        let blocks: *mut BlockList = addr_of_mut!((*this).blocks);

        // Invariant: We have at least one block
        assert!((*blocks).len() > 1, "cannot free unused blocks with len=1");

        // Any block past `current.limit` is unused
        let current_limit: *mut Opaque = (*this).limit;
        let mut index = (*blocks).len() - 1;
        while block_limit(blocks, index) != current_limit {
            manually_drop_block_at(blocks, index);
            index -= 1;
        }
        debug!("free {n} blocks", n = (*blocks).len() - index + 1);
        (*blocks).set_len(index + 1);
    }
}

pub struct HandleScope<'ctx> {
    scope_data: *mut HandleScopeData,
    allocator: *const Allocator,

    prev_next: *mut Opaque,
    prev_limit: *mut Opaque,
    level: usize,

    lifetime: PhantomData<fn(&'ctx ()) -> &'ctx ()>,
}

impl<'ctx> HandleScope<'ctx> {
    pub(crate) unsafe fn new(
        scope_data: *mut HandleScopeData,
        allocator: *const Allocator,
    ) -> Self {
        let prev_next = (*scope_data).next;
        let prev_limit = (*scope_data).limit;
        let level = (*scope_data).level;
        (*scope_data).level += 1;

        debug!("prev_next={prev_next:p}, prev_limit={prev_limit:p}, level={level}");

        HandleScope {
            scope_data,
            allocator,
            prev_next,
            prev_limit,
            level,
            lifetime: PhantomData,
        }
    }

    #[inline]
    pub fn alloc<T: Trace>(&self, data: T) -> Handle<'_, T> {
        unsafe {
            let ptr = (*self.allocator).alloc(data);
            Handle::new(self, ptr)
        }
    }
}

impl<'ctx> Drop for HandleScope<'ctx> {
    fn drop(&mut self) {
        unsafe {
            // Reset to previous scope
            let scope_data = self.scope_data;
            (*scope_data).next = self.prev_next;
            (*scope_data).level -= 1;

            debug!(
                "data.next={next:p}, data.level={level}",
                next = (*scope_data).next,
                level = (*scope_data).level
            );

            // handle scopes must be created and dropped in stack order
            assert_eq!((*scope_data).level, self.level);

            // If we have a different limit, then we must have allocated one or more new blocks
            // Free those now, because they're no longer being used
            // TODO: always keep one spare block
            if (*scope_data).limit != self.prev_limit {
                (*scope_data).limit = self.prev_limit;
                HandleScopeData::free_unused_blocks(scope_data);
            }
        }
    }
}

pub struct Handle<'scope, T> {
    /// Pointer to the handle slot which contains the actual memory location of `T`.
    slot: *mut Opaque,

    lifetime: PhantomData<fn(&'scope T) -> &'scope T>,
}

impl<'scope, T> Handle<'scope, T> {
    pub(crate) unsafe fn new(scope: &'scope HandleScope<'_>, ptr: *mut GcCell<T>) -> Self {
        // Grow if needed
        let scope_data = scope.scope_data;
        if (*scope_data).next == (*scope_data).limit {
            HandleScopeData::allocate_block(scope_data);
        }

        // The actual allocation (pointer bump)
        let slot = (*scope_data).next;
        (*scope_data).next = (*scope_data).next.add(1);

        // Initialize the slot
        *slot = ptr as Opaque;

        debug!(
            "{slot:p} = {ptr:p}, next = {next:p}",
            next = (*scope_data).next
        );

        Handle {
            slot,
            lifetime: PhantomData,
        }
    }
}

impl<'scope, T> Deref for Handle<'scope, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*(self.slot.read().cast::<T>()) }
    }
}
