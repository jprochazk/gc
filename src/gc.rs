use crate::alloc::Allocator;
use crate::alloc::GcCell;
use crate::handle::Scope;
use crate::handle::ScopeData;
use std::cell::UnsafeCell;
use std::ptr::null_mut;

pub trait Trace {
    /// # Safety
    /// The implementation _must_ trace all interior references.
    unsafe fn trace(&self);
}

pub struct Gc {
    scope_data: UnsafeCell<ScopeData>,
    allocator: UnsafeCell<Allocator>,
}

impl Gc {
    pub fn new(config: Config) -> Self {
        Self {
            scope_data: UnsafeCell::new(ScopeData::new()),
            allocator: UnsafeCell::new(Allocator::new(config.allocator)),
        }
    }

    #[inline]
    pub fn scope<'ctx, F>(&'ctx self, f: F)
    where
        F: for<'id> FnOnce(&'id Scope<'ctx>),
    {
        unsafe {
            assert_eq!((*self.scope_data.get()).level, 0);
            let scope = Scope::new(self.scope_data.get(), self.allocator.get());
            f(&scope);
        }
    }

    #[inline]
    pub fn collect(&self) {
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
}

fn mark(scope_data: *mut ScopeData) {
    debug!("mark phase");

    unsafe {
        // trace all roots
        // for us that's only live handles
        let scope_data = &*scope_data;
        'iter: for block in &scope_data.blocks {
            debug!(
                "live handles: {handles:?}",
                handles = block
                    .as_slice()
                    .iter()
                    .take_while(|&handle| !std::ptr::addr_eq(handle, scope_data.next))
                    .map(|handle| (DebugPtr(handle), DebugPtr(*handle)))
                    .collect::<Vec<_>>()
            );
            for handle in block.as_slice().iter() {
                if std::ptr::addr_eq(handle, scope_data.next) {
                    break 'iter;
                }

                if (*handle).is_null() {
                    continue;
                }

                debug!("visit handle {handle:p}");
                GcCell::trace(*handle);
            }
        }
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
            }
            current = prev;
        }

        match new_head {
            Some(ptr) => allocator.head.set(ptr),
            // we've freed every object
            None => allocator.head.set(null_mut()),
        }
    }
}

impl<'gc, T: Trace + 'gc> Trace for crate::handle::Heap<'gc, T> {
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
    use crate::handle::Heap;
    use crate::handle::Local;
    use crate::handle::LocalMut;
    use crate::handle::TransmuteLifetime;
    use std::cell::RefCell;

    struct Test {
        value: u32,
    }

    impl Trace for Test {
        unsafe fn trace(&self) {}
    }

    #[test]
    fn simple() {
        let cx = Gc::default();

        cx.scope(|s| {
            let v = s.alloc(Test { value: 100 });
            assert_eq!(v.value, 100);
        });
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

        let cx = Gc::default();

        cx.scope(|cx| {
            let a = cx.alloc(Test { value: 200 });
            let b = cx.alloc(Test { value: 300 });
            assert_eq!(a.value, 200);
            assert_eq!(b.value, 300);
            let c = cx.alloc(Test { value: 400 });
            assert_eq!(c.value, 400);
        });

        cx.collect();
    }

    #[test]
    fn mark_and_sweep_1() {
        // don't automatically trigger GC in this case
        let cx = Gc::new(Config::default().stress(false));

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

        cx.scope(|cx| {
            let a = cx.alloc(Test { value: 100 });

            cx.scope(|cx| {
                cx.alloc(Test { value: 200 });
            });

            let c = cx.alloc(Test { value: 300 });

            cx.scope(|cx| {
                let d = cx.alloc(Test { value: 400 });
                _ = d;
            });

            cx.scope(|cx| {
                let e = cx.alloc(Test { value: 500 });
                _ = e;
            });

            let f = cx.alloc(Test { value: 600 });

            assert_eq!(a.value + c.value + f.value, 1000);

            // run the GC while A, C, F are live
            cx.collect();
        });
    }

    struct Compound<'gc> {
        data: Heap<'gc, Test>,
    }

    impl<'gc> Trace for Compound<'gc> {
        unsafe fn trace(&self) {
            self.data.trace()
        }
    }

    #[test]
    fn mark_and_sweep_2() {
        // don't automatically trigger GC in this case
        let cx = Gc::new(Config::default().stress(false));

        cx.scope(|cx| {
            // gc...

            let data = cx.alloc(Test { value: 100 }).to_heap();
            let v = cx.alloc(Compound { data });

            let data = v.data.to_local(cx);
            assert_eq!(data.value, 100);
        })
    }

    thread_local! {
        static COLLECTED_NODES: RefCell<Vec<u32>> = const { RefCell::new(vec![]) };
    }

    struct Node<'gc> {
        prev: RefCell<Option<Heap<'gc, Node<'gc>>>>,
        next: RefCell<Option<Heap<'gc, Node<'gc>>>>,
        value: u32,
    }

    impl<'gc> Node<'gc> {
        fn new(cx: &'gc Scope<'_>, value: u32) -> Local<'gc, Node<'gc>> {
            cx.alloc(Node {
                prev: RefCell::new(None),
                next: RefCell::new(None),
                value,
            })
        }
    }

    impl<'gc> Trace for Node<'gc> {
        unsafe fn trace(&self) {
            self.prev.trace();
            self.next.trace();
        }
    }

    unsafe impl<'gc> TransmuteLifetime for Node<'gc> {
        type Output<'a> = Node<'a>;
    }

    impl<'gc> Drop for Node<'gc> {
        fn drop(&mut self) {
            COLLECTED_NODES.with_borrow_mut(|v| v.push(self.value));
        }
    }

    fn node_join<'gc>(left: Local<'gc, Node<'gc>>, right: Local<'gc, Node<'gc>>) {
        *left.next.borrow_mut() = Some(right.to_heap());
        *right.prev.borrow_mut() = Some(left.to_heap());
    }

    fn node_rotate_right<'gc>(node: &mut LocalMut<'gc, Node<'gc>>) -> bool {
        let next = *node.next.borrow();
        if let Some(next) = next {
            next.move_to(node);
            true
        } else {
            false
        }
    }

    fn node_rotate_left<'gc>(node: &mut LocalMut<'gc, Node<'gc>>) -> bool {
        let prev = *node.prev.borrow();
        if let Some(prev) = prev {
            prev.move_to(node);
            true
        } else {
            false
        }
    }

    #[test]
    fn escape_value() {
        let cx = Gc::default();

        cx.scope(|cx| {
            let node = cx.escape(|cx| Node::new(cx, 1));

            cx.collect();

            assert_eq!(node.value, 100);
        });
    }

    #[test]
    fn doubly_linked_list() {
        COLLECTED_NODES.with_borrow_mut(|v| v.clear());

        let cx = Gc::default();

        cx.scope(|cx| {
            // 1 <-> 2 <-> 3 <-> 4
            let mut root = cx.empty_slot();
            cx.scope(|cx| {
                let one = Node::new(cx, 1);
                let two = Node::new(cx, 2);
                let three = Node::new(cx, 3);
                let four = Node::new(cx, 4);

                node_join(one, two);
                node_join(two, three);
                node_join(three, four);

                root.set(one);
            });
            let root = root.get().unwrap();

            // check that we can traverse the linked list in both directions
            cx.scope(|cx| {
                // TODO: allow reusing the same local slot
                let mut root = root.in_scope_mut(cx);
                for i in 1..=4 {
                    assert_eq!(root.value, i);
                    node_rotate_right(&mut root);
                }
                for i in (1..=4).rev() {
                    assert_eq!(root.value, i);
                    node_rotate_left(&mut root);
                }
            });

            // make the list circular, and remove reference to `4`
            cx.scope(|cx| {
                // from:
                //     1 <-> 2 <-> 3 <-> 4
                // to:
                // +-> 1 <-> 2 <-> 3 <-+ 4
                // |___________________|

                let one = root.in_scope(cx);
                let two = one.next.borrow().unwrap().to_local(cx);
                let three = two.next.borrow().unwrap().to_local(cx);
                node_join(three, one);
            });

            // `4` is now unreachable, and will be collected during the next GC:
            cx.collect();

            assert!(COLLECTED_NODES.with_borrow(|v| v == &[4]));

            // rotating through a circular list will yield the same values N times:
            cx.scope(|cx| {
                let mut root = root.in_scope_mut(cx);
                for _ in 0..2 {
                    for i in 1..=3 {
                        assert_eq!(root.value, i);
                        assert!(node_rotate_right(&mut root));
                    }
                }
            })
        });
    }

    /* fn _test_compile_fail() {
        let cx = Gc::default();

        let mut out: Local<'_, Test> = unsafe { std::mem::MaybeUninit::uninit().assume_init() };
        cx.scope(|cx| {
            out = cx.alloc(Test { value: 100 });
            cx.scope(|cx| {})
        })
    } */
}
