//! Heap-allocated GC handles.
//!
//! Each `Local<T>` is a double indirection, similar to `*mut *mut T`.
//! The first pointer is to the slot in the handle block, and the second
//! to the actual value on the GC heap.
//!
//! The concept is similar to a shadow stack. We track which objects on the
//! heap are referenced from the stack by holding the actual references to
//! objects in a heap-allocated block. This heap-allocated block is
//! treated as part of the root set by the GC, so the referenced objects are
//! always considered live.
//!
//! That means having a `Local<T>` allows derefencing to the `T`, without
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
use crate::gc::gc;
use crate::gc::Trace;
use std::marker::PhantomData;
use std::ops::Deref;
use std::ptr::addr_of_mut;
use std::ptr::null_mut;

// TODO: Rework this to use the same API as rusty_v8.

// Surely pages are at least 4kB!
const BLOCK_SIZE: usize = 4096 / std::mem::size_of::<Opaque>();

type HeapRef<T> = *mut GcCell<T>;
type Opaque = HeapRef<Data>;

type BlockList = Vec<Box<[Opaque; BLOCK_SIZE]>>;

type Invariant<'a, T = ()> = PhantomData<fn(&'a T) -> &'a T>;

pub trait Object: Sized + 'static {}

pub(crate) struct ScopeData {
    /// Next available slot in the current block.
    ///
    /// None left if `next == limit`.
    pub(crate) next: *mut Opaque,

    /// End of the current block.
    pub(crate) limit: *mut Opaque,

    /// Scope nesting depth.
    pub(crate) level: usize,

    /// Last used block contains `scope_data.next`.
    pub(crate) blocks: BlockList,
}

impl ScopeData {
    pub(crate) fn new() -> Self {
        let mut this = Self {
            next: null_mut(),
            limit: null_mut(),
            level: 0,
            blocks: BlockList::new(),
        };
        unsafe {
            ScopeData::allocate_block(&mut this);
        }
        this
    }
}

impl ScopeData {
    #[cold]
    #[inline(never)]
    unsafe fn allocate_block(this: *mut ScopeData) {
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
    unsafe fn free_unused_blocks(this: *mut ScopeData) {
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

pub struct Scope<'ctx> {
    scope_data: *mut ScopeData,
    allocator: *mut Allocator,

    prev_next: *mut Opaque,
    prev_limit: *mut Opaque,
    level: usize,

    #[allow(unused)]
    lifetime: Invariant<'ctx>,
}

impl<'ctx> Scope<'ctx> {
    pub(crate) unsafe fn new(scope_data: *mut ScopeData, allocator: *mut Allocator) -> Self {
        let prev_next = (*scope_data).next;
        let prev_limit = (*scope_data).limit;
        let level = (*scope_data).level;
        (*scope_data).level += 1;

        debug!("prev_next={prev_next:p}, prev_limit={prev_limit:p}, level={level}");

        Scope {
            scope_data,
            allocator,
            prev_next,
            prev_limit,
            level,
            lifetime: PhantomData,
        }
    }

    #[inline]
    pub fn scope<F>(&self, f: F)
    where
        F: for<'id> FnOnce(&'id Scope<'ctx>),
    {
        unsafe {
            f(&Scope::new(self.scope_data, self.allocator));
        };
    }

    #[inline]
    pub fn escape<'scope, F, R, T>(&'scope self, f: F) -> Local<'scope, T>
    where
        F: for<'id> FnOnce(&'id Scope<'ctx>) -> Local<'id, R>,
        R: TransmuteLifetime<Output<'scope> = T>,
    {
        unsafe {
            let mut slot = LocalMut::<'scope, T>::new(self, null_mut());
            let scope = Scope::new(self.scope_data, self.allocator);
            let value = f(&scope);
            let ptr = *value.slot;
            slot.set_raw(ptr as HeapRef<T>);
            slot.to_local()
        }
    }

    #[inline]
    pub fn alloc<'scope, T: Trace + 'scope>(&'scope self, data: T) -> Local<'scope, T> {
        unsafe {
            // TODO: trigger GC here if heap is somewhat full
            if (*self.allocator).config.stress {
                // stress-test the GC by running it before every allocation
                super::gc::gc(self.scope_data, self.allocator);
            }

            assert!(self.is_active(), "alloc outside of current handle scope");
            let ptr = (*self.allocator).alloc(data);
            Local::new(self, ptr)
        }
    }

    pub fn empty_slot<'scope, T: Trace + 'scope>(&'scope self) -> LocalMutOpt<'scope, T> {
        unsafe { LocalMutOpt::new(self, null_mut()) }
    }

    /// Trigger a GC cycle.
    #[inline]
    pub fn collect(&self) {
        gc(self.scope_data, self.allocator)
    }

    #[inline]
    pub(crate) fn is_active(&self) -> bool {
        unsafe {
            let current_level = (*self.scope_data).level;
            current_level == self.level + 1
        }
    }
}

impl<'ctx> Drop for Scope<'ctx> {
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
                ScopeData::free_unused_blocks(scope_data);
            }
        }
    }
}

pub struct Heap<'scope, T> {
    pub(crate) ptr: HeapRef<T>,

    lifetime: Invariant<'scope, T>,
}

impl<'scope, T> Clone for Heap<'scope, T> {
    #[allow(clippy::non_canonical_clone_impl)]
    #[inline]
    fn clone(&self) -> Self {
        Self {
            ptr: self.ptr,
            lifetime: PhantomData,
        }
    }
}

impl<'scope, T> Copy for Heap<'scope, T> {}

impl<'scope, T> Heap<'scope, T> {
    #[inline]
    pub fn to_local<'a>(&self, scope: &'a Scope<'_>) -> Local<'a, T> {
        unsafe { Local::new(scope, self.ptr) }
    }

    #[inline]
    pub fn move_to(&self, local: &mut LocalMut<'_, T>) {
        unsafe { local.set_raw(self.ptr) }
    }
}

pub struct Local<'scope, T> {
    /// Pointer to the handle slot which contains the actual memory location of `T`.
    slot: *mut HeapRef<T>,

    lifetime: Invariant<'scope, T>,
}

impl<'scope, T> Local<'scope, T> {
    pub(crate) unsafe fn new(scope: &'scope Scope<'_>, ptr: HeapRef<T>) -> Self {
        // Grow if needed
        let scope_data = scope.scope_data;
        if (*scope_data).next == (*scope_data).limit {
            ScopeData::allocate_block(scope_data);
        }

        // The actual allocation (pointer bump)
        let slot = (*scope_data).next;
        (*scope_data).next = (*scope_data).next.add(1);

        // Initialize the slot
        let slot = slot.cast::<HeapRef<T>>();
        *slot = ptr;

        debug!(
            "{slot:p} = {ptr:p}, next = {next:p}",
            next = (*scope_data).next
        );

        Local {
            slot,
            lifetime: PhantomData,
        }
    }

    pub fn to_heap(self) -> Heap<'scope, T> {
        unsafe {
            Heap {
                ptr: *self.slot,
                lifetime: PhantomData,
            }
        }
    }

    pub fn in_scope<'a, R>(self, scope: &'a Scope<'_>) -> Local<'a, R>
    where
        T: TransmuteLifetime<Output<'a> = R>,
    {
        unsafe {
            let ptr = *self.slot;
            Local::new(scope, ptr as HeapRef<R>)
        }
    }

    pub fn in_scope_mut<'a, R>(self, scope: &'a Scope<'_>) -> LocalMut<'a, R>
    where
        T: TransmuteLifetime<Output<'a> = R>,
    {
        unsafe {
            let ptr = *self.slot;
            LocalMut::new(scope, ptr as HeapRef<R>)
        }
    }
}

impl<'scope, T: Trace> Deref for Local<'scope, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*GcCell::data(self.slot.read()) }
    }
}

impl<'scope, T> Clone for Local<'scope, T> {
    #[allow(clippy::non_canonical_clone_impl)]
    #[inline]
    fn clone(&self) -> Self {
        Self {
            slot: self.slot,
            lifetime: PhantomData,
        }
    }
}

impl<'scope, T> Copy for Local<'scope, T> {}

pub struct LocalMut<'scope, T> {
    inner: Local<'scope, T>,
}

impl<'scope, T> LocalMut<'scope, T> {
    pub(crate) unsafe fn new(scope: &'scope Scope<'_>, ptr: HeapRef<T>) -> Self {
        Self {
            inner: Local::new(scope, ptr),
        }
    }

    pub fn to_local(self) -> Local<'scope, T> {
        self.inner
    }

    pub fn set(&mut self, to: Local<'scope, T>) {
        unsafe { self.set_raw(*to.slot) }
    }

    pub(crate) unsafe fn set_raw(&mut self, ptr: HeapRef<T>) {
        *self.inner.slot = ptr;
    }
}

impl<'scope, T: Trace> Deref for LocalMut<'scope, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.inner.deref()
    }
}

pub struct LocalMutOpt<'scope, T> {
    inner: Local<'scope, T>,
}

impl<'scope, T> LocalMutOpt<'scope, T> {
    pub(crate) unsafe fn new(scope: &'scope Scope<'_>, ptr: HeapRef<T>) -> Self {
        Self {
            inner: Local::new(scope, ptr),
        }
    }

    pub fn set<'a, V>(&mut self, v: Local<'a, V>)
    where
        V: TransmuteLifetime<Output<'scope> = T>,
    {
        unsafe {
            let ptr = *v.slot;
            self.set_raw(ptr as HeapRef<T>)
        }
    }

    pub(crate) unsafe fn set_raw(&mut self, v: HeapRef<T>) {
        unsafe {
            *self.inner.slot = v;
        }
    }

    pub fn get(self) -> Option<Local<'scope, T>> {
        unsafe {
            let ptr = *self.inner.slot;
            if ptr.is_null() {
                return None;
            }

            Some(self.inner)
        }
    }
}

#[doc(hidden)]
pub unsafe trait TransmuteLifetime {
    type Output<'a>;
}
