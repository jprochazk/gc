use crate::alloc::Allocator;
use crate::alloc::GcCell;
use crate::handle::HandleScope;
use crate::handle::HandleScopeData;
use std::cell::UnsafeCell;
use std::ptr::null_mut;

pub trait Trace {
    fn trace(&self);
}

pub struct Gc {
    scope_data: UnsafeCell<HandleScopeData>,
    allocator: Allocator,
}

impl Gc {
    pub fn new() -> Self {
        Self {
            scope_data: UnsafeCell::new(HandleScopeData::new()),
            allocator: Allocator::new(),
        }
    }

    #[inline]
    pub fn scope<F, R>(&mut self, f: F) -> R
    where
        F: for<'id> FnOnce(HandleScope<'id>) -> R,
    {
        let handle_scope = unsafe { HandleScope::new(self.scope_data.get(), &self.allocator) };
        f(handle_scope)
    }

    #[cold]
    #[inline(never)]
    pub fn gc(&self) {
        self.mark();
        self.sweep();
    }

    fn mark(&self) {
        debug!("mark phase");

        unsafe {
            // trace all roots
            // for us that's only live handles
            let scope_data = &*self.scope_data.get();
            'iter: for block in &scope_data.blocks {
                for handle in block.as_slice().iter() {
                    debug!("visit handle {handle:p}");
                    if (*handle).is_null() {
                        continue;
                    }

                    GcCell::trace(*handle);

                    if std::ptr::addr_eq(handle, scope_data.next) {
                        break 'iter;
                    }
                }
            }
        }
    }

    fn sweep(&self) {
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
            // last marked object, which will have its `prev` pointer updated as we sweep dead objects
            let mut last_live = null_mut();

            // the new allocator head, which will be set to the first marked object
            let mut new_head = None;

            // current pointer in the linked list
            let mut current = self.allocator.head.get();

            while !current.is_null() {
                let prev = GcCell::get_prev(current);
                let marked = GcCell::is_marked(current);

                debug!(
                    "marked={marked}, current={current:p}, prev={prev:p}, last_live={last_live:p}"
                );

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
                Some(ptr) => self.allocator.head.set(ptr),
                // we've freed every object
                None => self.allocator.head.set(null_mut()),
            }
        }
    }
}

impl Drop for Gc {
    fn drop(&mut self) {
        unsafe {
            let mut current = self.allocator.head.get();
            while !current.is_null() {
                let prev = GcCell::get_prev(current);
                GcCell::free(current);
                current = prev;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Test {
        value: u32,
    }

    impl Trace for Test {
        fn trace(&self) {}
    }

    #[test]
    fn use_context() {
        let mut ctx = Gc::new();
        ctx.scope(|s| {
            let v = s.alloc(Test { value: 100 });
            println!("{}", v.value);
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

        let mut ctx = Gc::new();
        ctx.scope(|s| {
            let a = s.alloc(Test { value: 200 });
            let b = s.alloc(Test { value: 300 });
            println!("{}", a.value);
            println!("{}", b.value);
            let c = s.alloc(Test { value: 400 });
            println!("{}", c.value);
        });

        ctx.gc();
    }
}
