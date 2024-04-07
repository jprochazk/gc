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
//! A handle solve the problem by ensuring that the GC is always aware of `a`.
//!
//! A nice side-effect of this scheme is that it enables using any precise garbage
//! collection algorithm under the hood.

/*

#[derive(Object)]
struct Foo {
    bar: Heap<Bar>,
}

#[derive(Object)]
struct Bar {
    value: Cell<usize>,
}

fn main() {
    let mut gc = Gc::new();

    let mut scope = gc.context().handle_scope();

    let foo = scope.new(Foo {
        bar: scope.new(Bar {
            value: Cell::new(100),
        }).into()
    });
}

*/

use std::cell::UnsafeCell;
use std::marker::PhantomData;
use std::ops::Deref;
use std::ptr::addr_of_mut;
use std::ptr::null_mut;

// Surely pages are at least 4kB!
const BLOCK_SIZE: usize = 4096 / std::mem::size_of::<OpaquePtr>();

type HandleBlock = [OpaquePtr; BLOCK_SIZE];

type OpaquePtr = *mut ();

type BlockList = Vec<Box<HandleBlock>>;

pub trait Object: Sized + 'static {}

pub struct Context {
    scope_data: UnsafeCell<HandleScopeData>,
}

impl Context {
    pub fn new() -> Self {
        Self {
            scope_data: UnsafeCell::new(HandleScopeData {
                next: null_mut(),
                limit: null_mut(),
                level: 0,
                blocks: BlockList::new(),
            }),
        }
    }

    fn handle_scope(&mut self) -> HandleScope<'_> {
        unsafe { HandleScope::new(self) }
    }
}

impl Default for Context {
    fn default() -> Self {
        Self::new()
    }
}

struct HandleScopeData {
    /// The next available handle slot.
    ///
    /// None left if `next == limit`.
    next: *mut OpaquePtr,

    /// End of the handle block.
    limit: *mut OpaquePtr,

    /// Handle scope nesting depth.
    level: usize,

    /// Handle block list, the last used block is the one pointed to by `scope_data`.
    blocks: BlockList,
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

        (*this).next = next;
        (*this).limit = limit;
    }

    #[cold]
    #[inline(never)]
    unsafe fn free_unused_blocks(this: *mut HandleScopeData) {
        #[inline(always)]
        unsafe fn block_limit(blocks: *mut BlockList, index: usize) -> *mut OpaquePtr {
            let slice = Box::into_raw((*blocks).as_mut_ptr().add(index).read());
            let start = slice as *mut OpaquePtr;
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
        let current_limit: *mut OpaquePtr = (*this).limit;
        let mut index = (*blocks).len() - 1;
        while block_limit(blocks, index) != current_limit {
            manually_drop_block_at(blocks, index);
            index -= 1;
        }
        (*blocks).set_len(index + 1);
    }
}

pub struct HandleScope<'ctx> {
    ctx: *mut Context,
    prev_next: *mut OpaquePtr,
    prev_limit: *mut OpaquePtr,

    lifetime: PhantomData<fn(&'ctx ()) -> &'ctx ()>,
}

impl<'ctx> HandleScope<'ctx> {
    unsafe fn new(ctx: &'ctx mut Context) -> Self {
        let current = ctx.scope_data.get();
        let prev_next = (*current).next;
        let prev_limit = (*current).limit;
        (*current).level += 1;

        HandleScope {
            ctx,
            prev_next,
            prev_limit,
            lifetime: PhantomData,
        }
    }

    unsafe fn data(&mut self) -> *mut HandleScopeData {
        addr_of_mut!((*self.ctx).scope_data) as *mut HandleScopeData
    }
}

impl<'ctx> Drop for HandleScope<'ctx> {
    fn drop(&mut self) {
        unsafe {
            // Reset to previous scope
            let scope_data = self.data();
            (*scope_data).next = self.prev_next;
            (*scope_data).level -= 1;

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
    slot: *mut OpaquePtr,

    lifetime: PhantomData<fn(&'scope T) -> &'scope T>,
}

impl<'scope, T> Handle<'scope, T> {
    unsafe fn new(scope: &'scope mut HandleScope<'_>, ptr: *mut T) -> Self {
        // Grow if needed
        let scope_data = scope.data();
        if (*scope_data).next == (*scope_data).limit {
            HandleScopeData::allocate_block(scope_data);
        }

        // The actual allocation (pointer bump)
        let slot = (*scope_data).next;
        (*scope_data).next = (*scope_data).next.add(1);

        // Initialize the slot
        *slot = ptr as OpaquePtr;

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
