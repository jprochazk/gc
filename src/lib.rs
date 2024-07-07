#![allow(clippy::new_without_default, clippy::comparison_chain)]

extern crate self as gc;

#[macro_use]
mod macros;

mod alloc;
mod handle;

use alloc::Allocator;
use alloc::GcCell;
use handle::ScopeData;
use std::cell::UnsafeCell;
use std::ptr::null_mut;

/// Implementations of this trait should be derived using the `trace` attribute macro if possible.
pub trait Trace: 'static {
    /// ## Safety
    /// The implementation _must_ trace all interior references.
    unsafe fn trace(&self);
}

pub struct Gc {
    pub(crate) scope_data: UnsafeCell<ScopeData>,
    pub(crate) allocator: UnsafeCell<Allocator>,
}

impl Gc {
    pub fn new(config: Config) -> Self {
        Self {
            scope_data: UnsafeCell::new(ScopeData::new()),
            allocator: UnsafeCell::new(Allocator::new(config.allocator)),
        }
    }

    #[inline]
    pub fn collect(&mut self) {
        // TODO: incremental collection
        self.collect_all()
    }

    #[inline]
    pub fn collect_all(&mut self) {
        gc(self.scope_data.get(), self.allocator.get())
    }
}

impl Default for Gc {
    fn default() -> Self {
        Self::new(Config::default())
    }
}

impl Drop for Gc {
    fn drop(&mut self) {
        unsafe {
            let mut current = self.allocator.get_mut().head.get();
            while !current.is_null() {
                let prev = GcCell::get_prev(current);
                GcCell::free(current);
                current = prev;
            }
        }
    }
}

#[derive(Clone, Copy)]
pub struct Config {
    allocator: crate::alloc::Config,
}

impl Config {
    pub fn stress(mut self, v: bool) -> Self {
        self.allocator.stress = v;
        self
    }
}

#[allow(clippy::derivable_impls)]
impl Default for Config {
    fn default() -> Self {
        Self {
            allocator: crate::alloc::Config::default(),
        }
    }
}

#[inline(never)]
pub(crate) fn gc(scope_data: *mut ScopeData, allocator: *mut Allocator) {
    mark(scope_data);
    sweep(allocator);

    unsafe {
        (*scope_data).free_unused_blocks();
    }
}

fn mark(scope_data: *mut ScopeData) {
    debug!("mark phase");

    let scope_data = unsafe { &mut *scope_data };
    for cell in scope_data.iter() {
        if cell.is_null() {
            debug!("null handle");
            continue;
        }

        unsafe { GcCell::trace(cell) };
    }
}

#[cfg(__verbose_gc)]
struct DebugPtr<T>(*const T);
#[cfg(__verbose_gc)]
impl<T> std::fmt::Debug for DebugPtr<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:p}", self.0)
    }
}

fn sweep(allocator: *mut Allocator) {
    debug!("sweep phase");

    // the core of the algorithm is:
    //
    // ```
    // if marked(ptr):
    //   unmark(ptr)
    // else:
    //   free(ptr)
    // ```
    //
    // every object has a pointer to the object allocated before it,
    // which we use to traverse all objects. during sweep, we also
    // have to maintain it by updating the `prev` pointers of all
    // live objects to the next live object found when traversing
    // the list.
    //
    // before:
    //   null <- A <- B <- C <- D <- E
    //   mark:   1    1    0    0    1
    //
    // after:
    //   null <- A <- B <- E
    //   mark:   0    0    0

    unsafe {
        #[allow(unused_variables)]
        let mut freed_n = 0;

        let allocator = &*allocator;

        // last marked object, which will have its `prev` pointer updated as we sweep dead objects
        let mut last_live = null_mut();

        // the new allocator head, which will be set to the first marked object
        let mut new_head = None;

        // current pointer in the linked list
        let mut current = allocator.head.get();

        while !current.is_null() {
            let prev = GcCell::get_prev(current);
            let marked = GcCell::is_marked(current);

            debug!("marked={marked}, current={current:p}, prev={prev:p}, last_live={last_live:p}");

            if marked {
                GcCell::set_mark(current, false);
                last_live = current;
                if new_head.is_none() {
                    new_head = Some(current);
                }
            } else {
                if !last_live.is_null() {
                    GcCell::set_prev(last_live, prev);
                }
                GcCell::free(current);
                freed_n += 1;
            }
            current = prev;
        }

        match new_head {
            Some(ptr) => allocator.head.set(ptr),
            // we've freed every object
            None => allocator.head.set(null_mut()),
        }

        debug!("freed {freed_n} objects");
    }
}

impl Trace for () {
    unsafe fn trace(&self) {}
}

impl<T: Trace> Trace for crate::handle::Member<T> {
    unsafe fn trace(&self) {
        GcCell::trace(GcCell::erase(self.ptr).cast_const())
    }
}

impl<T: Trace> Trace for Option<T> {
    unsafe fn trace(&self) {
        if let Some(v) = self {
            v.trace();
        }
    }
}

impl<T: Trace> Trace for std::cell::RefCell<T> {
    unsafe fn trace(&self) {
        self.borrow().trace();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handle::EscapeScope;
    use crate::handle::Local;
    use crate::handle::Member;
    use crate::handle::Scope;
    use std::cell::RefCell;

    struct Test {
        value: u32,
    }

    impl Trace for Test {
        unsafe fn trace(&self) {}
    }

    #[test]
    fn simple() {
        let mut cx = Gc::default();
        let s = &mut Scope::new(&mut cx);
        let v = Local::new(s, Test { value: 100 });
        assert_eq!(v.value, 100);
    }

    #[test]
    fn mark_and_sweep_0() {
        // null <- A <- B <- C
        //         1    1    1
        //
        // (0) current: C, prev: B, C is live
        // (1) current: B, prev: A, B is live
        // (2) current: A, prev: null, A is live
        //
        // null <- A <- B <- C
        //         0    0    0

        let cx = &mut Gc::default();
        let s = &mut Scope::new(cx);
        let a = Local::new(s, Test { value: 100 });
        let b = Local::new(s, Test { value: 200 });
        assert_eq!(a.value, 100);
        assert_eq!(b.value, 200);
        let c = Local::new(s, Test { value: 300 });
        assert_eq!(c.value, 300);
        s.collect();
    }

    #[test]
    fn mark_and_sweep_1() {
        // don't automatically trigger GC in this case
        let mut cx = Gc::new(Config::default().stress(false));

        // null <- A <- B <- C <- D <- E <- F
        //         1    0    1    0    0    1
        //
        // (0) current: F, prev: E      F is marked, unmark
        // (1) current: E, prev: D      E is NOT marked, free, set F.prev = D
        // (2) current: D, prev: C      D is NOT marked, free, set F.prev = C
        // (3) current: C, prev: B      C is marked, unmark
        // (4) current: B, prev: A      B is NOT marked, free, set C.prev = A
        // (5) current: A, prev: null   A is marked, unmark
        //
        // null <- A <- C <- F
        //         0    0    0

        let s = &mut Scope::new(&mut cx);
        let a = Local::new(s, Test { value: 100 });
        {
            let s = &mut Scope::new(s);
            let _ = Local::new(s, Test { value: 200 });
        }
        let c = Local::new(s, Test { value: 300 });
        {
            let s = &mut Scope::new(s);
            let _ = Local::new(s, Test { value: 400 });
        }
        {
            let s = &mut Scope::new(s);
            let _ = Local::new(s, Test { value: 500 });
        }
        let f = Local::new(s, Test { value: 600 });
        assert_eq!(a.value + c.value + f.value, 1000);
        s.collect();
    }

    struct Compound {
        data: Member<Test>,
    }

    impl Trace for Compound {
        unsafe fn trace(&self) {
            self.data.trace()
        }
    }

    #[test]
    fn mark_and_sweep_2() {
        // don't automatically trigger GC in this case
        let mut cx = Gc::new(Config::default().stress(false));

        let s = &mut Scope::new(&mut cx);
        let data = Local::new(s, Test { value: 100 }).to_member();
        let v = Local::new(s, Compound { data });

        let data = unsafe { v.data.in_scope(s) };
        assert_eq!(data.value, 100);
    }

    thread_local! {
        static COLLECTED_NODES: RefCell<Vec<u32>> = const { RefCell::new(vec![]) };
    }

    struct Node {
        prev: RefCell<Option<Member<Node>>>,
        next: RefCell<Option<Member<Node>>>,
        value: u32,
    }

    impl Node {
        fn new<'a>(s: &mut Scope<'a>, value: u32) -> Local<'a, Node> {
            Local::new(
                s,
                Node {
                    prev: RefCell::new(None),
                    next: RefCell::new(None),
                    value,
                },
            )
        }
    }

    impl Trace for Node {
        unsafe fn trace(&self) {
            self.prev.trace();
            self.next.trace();
        }
    }

    impl Drop for Node {
        fn drop(&mut self) {
            COLLECTED_NODES.with_borrow_mut(|v| v.push(self.value));
        }
    }

    fn node_join<'a>(left: &Local<'a, Node>, right: &Local<'a, Node>) {
        *left.next.borrow_mut() = Some(right.to_member());
        *right.prev.borrow_mut() = Some(left.to_member());
    }

    fn node_rotate_right(node: &mut Local<'_, Node>) -> bool {
        let next = *node.next.borrow();
        if let Some(next) = next {
            unsafe {
                next.move_to(node);
            }
            true
        } else {
            false
        }
    }

    fn node_rotate_left(node: &mut Local<'_, Node>) -> bool {
        let prev = *node.prev.borrow();
        if let Some(prev) = prev {
            unsafe {
                prev.move_to(node);
            }
            true
        } else {
            false
        }
    }

    #[test]
    fn escape_value() {
        let mut cx = Gc::default();

        let outer = &mut Scope::new(&mut cx);
        let node: Local<Node> = {
            let inner = &mut EscapeScope::new(outer);
            let node = Node::new(inner, 1);
            inner.escape(node)
        };

        outer.collect();

        let foo = Node::new(outer, 20);

        assert_eq!(node.value, 1);
        assert_eq!(foo.value, 20);
    }

    #[test]
    fn tombstone_simple() {
        let cx = &mut Gc::default();

        let outer = &mut Scope::new(cx);
        let ptr: *mut *mut GcCell<Node> = {
            let inner = &mut Scope::new(outer);
            let node = Node::new(inner, 1);

            node.as_ptr()
        };

        outer.collect();

        // should still be live because it's below the tombstone
        // and we haven't re-used the handle yet
        assert_eq!(unsafe { (*GcCell::data(*ptr)).value }, 1);

        let _ = Node::new(outer, 2);

        // we re-used the handle for the new node, so the reference
        // is updated and so is the value stored in the object
        assert_eq!(unsafe { (*GcCell::data(*ptr)).value }, 2);
    }

    #[test]
    fn tombstone_nested() {
        COLLECTED_NODES.with_borrow_mut(|v| v.clear());

        let cx = &mut Gc::default();

        let scope0 = &mut Scope::new(cx);
        let _ = Node::new(scope0, 1);
        {
            let scope1 = &mut Scope::new(scope0);
            let _ = Node::new(scope1, 2);
            {
                let scope2 = &mut Scope::new(scope1);
                let _ = Node::new(scope2, 3);
            }
        }

        scope0.collect();

        COLLECTED_NODES.with_borrow(|v| assert_eq!(v, &[3]));
    }

    #[test]
    fn tombstone_next_block() {
        COLLECTED_NODES.with_borrow_mut(|v| v.clear());

        let cx = &mut Gc::default();

        let outer = &mut Scope::new(cx);
        let first = Node::new(outer, 1);
        {
            let inner = &mut Scope::new(outer);
            for _ in 0..crate::handle::BLOCK_SIZE {
                let _ = Node::new(inner, 2);
            }
        }
        let second = Node::new(outer, 3);

        // 1 handle apart
        let distance = (second.as_ptr() as usize - first.as_ptr() as usize) / 8;
        assert_eq!(distance, 1);
    }

    #[test]
    fn doubly_linked_list() {
        COLLECTED_NODES.with_borrow_mut(|v| v.clear());

        let mut cx = Gc::default();

        {
            let s = &mut Scope::new(&mut cx);

            let root = {
                let s = &mut EscapeScope::new(s);
                let one = Node::new(s, 1);
                let two = Node::new(s, 2);
                let three = Node::new(s, 3);
                let four = Node::new(s, 4);

                node_join(&one, &two);
                node_join(&two, &three);
                node_join(&three, &four);

                s.escape(one)
            };

            // check that we can traverse the linked list in both directions
            {
                let s = &mut Scope::new(s);
                let mut root = root.in_scope(s);
                for i in 1..=4 {
                    assert_eq!(root.value, i);
                    node_rotate_right(&mut root);
                }
                for i in (1..=4).rev() {
                    assert_eq!(root.value, i);
                    node_rotate_left(&mut root);
                }
            }

            // make the list circular, and remove the reference to `4`
            {
                // from:
                //     1 <-> 2 <-> 3 <-> 4
                // to:
                // +-> 1 <-> 2 <-> 3 <-+ 4
                // |___________________|

                let s = &mut Scope::new(s);
                let one = root.in_scope(s);
                unsafe {
                    let two = one.next.borrow().unwrap().in_scope(s);
                    let three = two.next.borrow().unwrap().in_scope(s);
                    node_join(&three, &one);
                }
            }

            s.collect();

            COLLECTED_NODES.with_borrow(|v| assert_eq!(v, &[4]));

            {
                // rotating through the circular list gets us an infinite loop:
                // 1 -> 2 -> 3 -> 1 -> 2 -> 3
                let s = &mut Scope::new(s);
                let mut root = root.in_scope(s);
                for _ in 0..2 {
                    for i in 1..=3 {
                        assert_eq!(root.value, i);
                        assert!(node_rotate_right(&mut root));
                    }
                }
            }
        }

        drop(cx);

        COLLECTED_NODES.with_borrow(|v| assert_eq!(v, &[4, 3, 2, 1]));
    }
}

pub(crate) fn default<T: Default>() -> T {
    T::default()
}
