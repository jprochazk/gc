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
use std::cmp;
use std::marker::PhantomData;
use std::ops::Deref;
use std::ops::DerefMut;
use std::ptr::null_mut;

// TODO: `project!` macro for safe access of `Member` fields through `Local`
// TODO: Cell-like types with write barries for interior mutability of reference fields
//       e.g. MemberMut<T> with `set` method that calls the write barrier
//       see `gc-arena` for how they do field projections and write barriers
// TODO: add `Global<T>` type, which is a reference-counted handle

// Surely pages are at least 4kB!
pub(crate) const BLOCK_SIZE: usize = 4096 / std::mem::size_of::<OpaquePtr>();

type Ptr<T> = *mut GcCell<T>;
type OpaquePtr = Ptr<Data>;

type Block = [OpaquePtr; BLOCK_SIZE];
type BlockList = Vec<Box<Block>>;

type Invariant<'a, T = ()> = PhantomData<fn(&'a T) -> &'a T>;
type Covariant<'a, T = ()> = PhantomData<&'a T>;

#[derive(Clone, Copy)]
struct Bump {
    index: u32,
    ptr: *mut OpaquePtr,
}

impl Default for Bump {
    fn default() -> Self {
        Self {
            index: 0,
            ptr: null_mut(),
        }
    }
}

pub struct ScopeData {
    /// Bump ptr in the current block.
    next: Bump,

    /// Stored `next` from the time of last scope drop.
    tombstone: Bump,

    /// End of the current block.
    ///
    /// No free handles left if `next.ptr == limit`,
    /// in which case new block must be allocated.
    limit: *mut OpaquePtr,

    /// Scope nesting depth.
    ///
    /// Only exists for debug purposes, to assert that
    /// scopes are pushed/popped in the right order.
    next_scope_level: usize,

    /// List of allocated blocks.
    ///
    /// Address stability of the list does not matter, so it is a simple `Vec`.
    ///
    /// Blocks _must not move_, so they are boxed independently.
    blocks: BlockList,
}

impl ScopeData {
    pub(crate) fn new() -> Self {
        let mut this = Self {
            next: Default::default(),
            tombstone: Default::default(),
            limit: null_mut(),
            next_scope_level: 0,
            blocks: BlockList::new(),
        };

        // Invariant: We must always have at least one block
        this.alloc_block();

        this
    }

    #[inline]
    fn alloc_handle(&mut self) -> *mut OpaquePtr {
        // Allocate new block if needed
        if self.next.ptr == self.limit {
            self.alloc_block();
        }

        // Allocate handle
        let handle = self.next.ptr;
        self.next.ptr = unsafe { self.next.ptr.add(1) };

        handle
    }

    #[cold]
    #[inline(never)]
    fn alloc_block(&mut self) {
        // Allocate new block
        self.blocks.push(Box::new([null_mut(); BLOCK_SIZE]));
        self.next = Bump {
            index: (self.blocks.len() - 1) as u32,
            ptr: self.blocks.last_mut().unwrap().as_mut_ptr(),
        };
        self.limit = unsafe { self.next.ptr.add(BLOCK_SIZE) };

        debug!(
            "index={}, ptr={:p}, limit={:p}",
            self.next.index, self.next.ptr, self.limit,
        );
    }

    #[cold]
    #[inline(never)]
    pub(crate) fn free_unused_blocks(&mut self) {
        let last_used_block = cmp::max(self.tombstone.index, self.next.index) as usize;
        drop(self.blocks.drain(last_used_block + 1..));
    }

    pub(crate) fn iter(&self) -> ScopeDataIter {
        debug!("iter");
        let end = if self.tombstone.index == self.next.index {
            debug!("tombstone.index == next.index");
            cmp::max(self.tombstone.ptr, self.next.ptr)
        } else if self.tombstone.index < self.next.index {
            debug!("tombstone.index < next.index");
            self.next.ptr
        } else {
            debug!("tombstone.index > next.index");
            self.tombstone.ptr
        };

        ScopeDataIter {
            scope_data: self,
            next_block_index: 1,
            next: self.blocks[0].as_ptr(),
            block_limit: unsafe { self.blocks[0].as_ptr().add(BLOCK_SIZE) },
            end,
            lifetime: PhantomData,
        }
    }
}

pub(crate) struct ScopeDataIter<'a> {
    scope_data: *const ScopeData,
    next_block_index: usize,
    next: *const OpaquePtr,
    block_limit: *const OpaquePtr,
    end: *const OpaquePtr,

    lifetime: Invariant<'a>,
}

impl ScopeDataIter<'_> {
    #[inline]
    fn data(&self) -> &ScopeData {
        unsafe { &*self.scope_data }
    }
}

impl Iterator for ScopeDataIter<'_> {
    type Item = OpaquePtr;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next == self.end {
            debug!("next=none");
            // at end - no more handles
            None
        } else if self.next == self.block_limit {
            debug!("next block, next=block[0]");

            // at block end - go to next block
            // we are guaranteed to have another block,
            // because we didn't find `self.end` in this one
            self.next = self.data().blocks[self.next_block_index].as_ptr();
            self.next_block_index += 1;
            self.block_limit = unsafe { self.next.add(BLOCK_SIZE) };

            Some(unsafe { *self.next })
        } else {
            debug!(
                "next=block[{}]",
                (self.next as usize - unsafe { self.block_limit.sub(BLOCK_SIZE) } as usize) / 8
            );

            // next handle in current block
            let next = self.next;
            self.next = unsafe { self.next.add(1) };
            Some(unsafe { *next })
        }
    }
}

pub struct Scope<'scope> {
    scope_data: *mut ScopeData,
    allocator: *mut Allocator,

    prev_next: Bump,
    level: usize,

    #[allow(unused)]
    lifetime: Invariant<'scope>,
}

impl<'scope> Scope<'scope> {
    pub fn new<'outer>(parent: &'scope mut impl ParentScope<'outer>) -> Self {
        unsafe { Self::new_raw(parent.scope_data(), parent.allocator()) }
    }

    pub(crate) unsafe fn new_raw(scope_data: *mut ScopeData, allocator: *mut Allocator) -> Self {
        let prev_next = (*scope_data).next;
        let level = (*scope_data).next_scope_level;
        (*scope_data).next_scope_level += 1;

        debug!(
            "prev_next={:p}, prev_limit={:p}, level={}",
            prev_next.ptr,
            (*scope_data).limit,
            level
        );

        Scope {
            scope_data,
            allocator,
            prev_next,
            level,
            lifetime: PhantomData,
        }
    }

    /// Trigger a GC cycle.
    #[inline]
    pub fn collect(&mut self) {
        // TODO: incremental collection
        self.collect_all()
    }

    /// Trigger a _full_ GC cycle.
    #[inline]
    pub fn collect_all(&mut self) {
        gc(self.scope_data, self.allocator)
    }

    #[inline]
    pub(crate) fn is_active(&self) -> bool {
        unsafe {
            let current_level = (*self.scope_data).next_scope_level;
            current_level == self.level + 1
        }
    }
}

impl<'ctx> Drop for Scope<'ctx> {
    fn drop(&mut self) {
        unsafe {
            // Reset to previous scope
            let scope_data = self.scope_data;
            (*scope_data).tombstone = (*scope_data).next;
            (*scope_data).next = self.prev_next;
            (*scope_data).next_scope_level -= 1;

            debug!(
                "\n  data.tombstone={tombstone:p}\n  data.next={next:p}\n  data.next_scope_level={level}",
                tombstone = (*scope_data).tombstone.ptr,
                next = (*scope_data).next.ptr,
                level = (*scope_data).next_scope_level
            );

            // handle scopes must be created and dropped in stack order
            assert_eq!((*scope_data).next_scope_level, self.level);
        }
    }
}

pub struct EscapeScope<'scope, 'outer> {
    scope: Scope<'scope>,
    local: Local<'outer, Data>,
    escaped: bool,
}

impl<'scope, 'outer> EscapeScope<'scope, 'outer> {
    pub fn new(parent: &'scope mut impl ParentScope<'outer>) -> Self {
        unsafe {
            let scope_data = parent.scope_data();
            let allocator = parent.allocator();

            // allocate the slot _before_ constructing a new scope
            let local = Local::alloc(scope_data, null_mut());
            let scope = Scope::new_raw(scope_data, allocator);

            EscapeScope {
                scope,
                local,
                escaped: false,
            }
        }
    }

    pub fn escape<T: Trace>(&mut self, value: Local<'scope, T>) -> Local<'outer, T> {
        assert!(!self.escaped, "cannot escape twice");
        self.escaped = true;

        unsafe {
            *self.local.slot = *value.slot as OpaquePtr;
            Local {
                slot: self.local.slot as *mut Ptr<T>,
                lifetime: PhantomData,
            }
        }
    }
}

impl<'scope, 'outer: 'scope> Deref for EscapeScope<'scope, 'outer> {
    type Target = Scope<'scope>;

    fn deref(&self) -> &Self::Target {
        &self.scope
    }
}

impl<'scope, 'outer: 'scope> DerefMut for EscapeScope<'scope, 'outer> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.scope
    }
}

#[inline]
fn alloc<'scope, T: Trace + 'scope>(scope: &mut impl ParentScope<'scope>, value: T) -> Ptr<T> {
    unsafe {
        let scope_data = scope.scope_data();
        let allocator = scope.allocator();

        // TODO: trigger GC here if heap is somewhat full
        if (*allocator).config.stress {
            // stress-test the GC by running it before every allocation
            super::gc::gc(scope_data, allocator);
        }

        assert!(scope.is_active(), "alloc outside of current handle scope");
        (*allocator).alloc(value)
    }
}

/// This struct wraps a pointer to an object.
///
/// Accessing a member is always unsafe, and care must be taken to ensure
/// it is accessed only when reachable through a root.
///
/// The easiest way to ensure this is to store the object in a `Local`.
///
/// It is safe to access members transitively through a `Local`:
///
/// If an object is in a `Local`, and its `Trace` implementation correctly traces
/// through all interior references, then members themselves do not need
/// to be placed in locals, as they will be reachable through their parent.
///
/// ## Example
/// ```rust,ignore
/// #[trace]
/// struct Foo {
///   bar: Member<Bar>,
/// }
///
/// impl Foo {
///   fn new(s: &mut Scope, bar: Local<Bar>) -> Local<Self> {
///     Local::new(s, Foo { bar: bar.into() })
///   }
/// }
///
/// let foo = {
///   let s = &mut EscapeScope::new(s);
///   let bar = Bar::new(s);
///   let foo = Foo::new(s, bar);
///   s.escape(foo)
/// };
/// // `bar` is no directly reachable from a `Local`,
/// // but it is safe to dereference as it is still
/// // reachable through `foo`:
/// let bar: &Bar = unsafe { foo.bar.get() };
/// ```
pub struct Member<T: Trace> {
    pub(crate) ptr: Ptr<T>,
}

impl<T: Trace> Member<T> {
    /// Dereference the inner pointer and obtain a reference to the object.
    ///
    /// ## Safety
    /// The object must not have been freed yet, and still be reachable.
    #[inline]
    pub unsafe fn get(&self) -> &T {
        &*GcCell::data(self.ptr)
    }

    /// Dereference the inner pointer and obtain a reference to the object.
    ///
    /// ## Safety
    /// The object must not have been freed yet, and still be reachable.
    #[inline]
    pub unsafe fn in_scope<'a>(self, scope: &mut Scope<'a>) -> Local<'a, T> {
        Local::alloc(scope.scope_data, self.ptr)
    }

    /// Dereference the inner pointer and obtain a reference to the object.
    ///
    /// ## Safety
    /// The object must not have been freed yet, and still be reachable.
    #[inline]
    pub unsafe fn move_to(self, local: &mut Local<'_, T>) {
        local.set_raw(self.ptr)
    }
}

impl<T: Trace> Clone for Member<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: Trace> Copy for Member<T> {}

pub struct Local<'scope, T: Trace> {
    /// Pointer to the handle slot which contains the actual memory location of `T`.
    slot: *mut Ptr<T>,

    lifetime: Covariant<'scope, T>,
}

impl<'scope, T: Trace> Local<'scope, T> {
    pub fn new(scope: &mut Scope<'scope>, value: T) -> Self
    where
        T: Trace + 'scope,
    {
        unsafe {
            // 1. allocate the object on the heap
            let ptr = alloc(scope, value);
            // 2. put it in a fresh handle
            Self::alloc(scope.scope_data, ptr)
        }
    }

    pub(crate) unsafe fn alloc(scope_data: *mut ScopeData, ptr: Ptr<T>) -> Self {
        let data = &mut *scope_data;
        let slot = data.alloc_handle() as *mut Ptr<T>;
        *slot = ptr;
        debug!("{slot:p} = {ptr:p}, next = {next:p}", next = data.next.ptr);

        Local {
            slot,
            lifetime: PhantomData,
        }
    }

    pub fn to_member(&self) -> Member<T> {
        unsafe { Member { ptr: *self.slot } }
    }

    // TODO: check that you can't leak call this on a scope that has a child scope
    pub fn in_scope<'a>(&self, scope: &mut Scope<'a>) -> Local<'a, T> {
        unsafe {
            let ptr = *self.slot;
            Local::alloc(scope.scope_data, ptr)
        }
    }

    pub fn set(&mut self, other: Local<'_, T>) {
        unsafe {
            *self.slot = *other.slot;
        }
    }

    pub(crate) unsafe fn set_raw(&mut self, other: Ptr<T>) {
        unsafe {
            *self.slot = other;
        }
    }

    #[allow(dead_code)]
    pub(crate) fn as_ptr(&self) -> *mut Ptr<T> {
        self.slot
    }
}

impl<'scope, T: Trace> Deref for Local<'scope, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*GcCell::data(self.slot.read()) }
    }
}

pub trait ParentScope<'scope>: private::Sealed {
    fn scope_data(&self) -> *mut ScopeData;
    fn allocator(&self) -> *mut Allocator;
    fn is_active(&self) -> bool;
}

mod private {
    pub trait Sealed {}
}

impl private::Sealed for Scope<'_> {}
impl<'scope> ParentScope<'scope> for Scope<'scope> {
    fn scope_data(&self) -> *mut ScopeData {
        self.scope_data
    }

    fn allocator(&self) -> *mut Allocator {
        self.allocator
    }

    fn is_active(&self) -> bool {
        self.is_active()
    }
}

impl private::Sealed for super::gc::Gc {}
impl ParentScope<'_> for super::gc::Gc {
    fn scope_data(&self) -> *mut ScopeData {
        self.scope_data.get()
    }

    fn allocator(&self) -> *mut Allocator {
        self.allocator.get()
    }

    fn is_active(&self) -> bool {
        false
    }
}
