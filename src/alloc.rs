use crate::gc::Trace;
use std::alloc::dealloc;
use std::alloc::Layout;
use std::cell::Cell;
use std::cell::UnsafeCell;
use std::mem::transmute;
use std::ptr::addr_of;
use std::ptr::addr_of_mut;
use std::ptr::null_mut;

pub struct Allocator {
    pub(crate) head: Cell<*mut GcCell<Data>>,
    pub(crate) config: Config,
}

impl Allocator {
    #[inline]
    pub(crate) fn new(config: Config) -> Self {
        Self {
            head: Cell::new(null_mut()),
            config,
        }
    }

    #[inline]
    pub(crate) fn alloc<T: Trace>(&self, data: T) -> *mut GcCell<T> {
        let ptr = Box::into_raw(Box::new(GcCell {
            header: GcHeader {
                prev: UnsafeCell::new(self.head.get()),
                vt: Vt::get::<T>(),
                mark: UnsafeCell::new(false),
            },
            data,
        }));
        debug!("alloc {ptr:p}");

        // TODO: maybe use `next` pointers instead of `prev`,
        // so the head doesn't need to be updated constantly?
        self.head.set(GcCell::erase(ptr));
        ptr
    }
}

#[derive(Clone, Copy)]
pub struct Config {
    pub stress: bool,
}

#[allow(clippy::derivable_impls)]
impl Default for Config {
    #[cfg(test)]
    fn default() -> Self {
        Self { stress: true }
    }

    #[cfg(not(test))]
    fn default() -> Self {
        Self { stress: false }
    }
}

pub struct GcCell<T: ?Sized> {
    header: GcHeader,
    data: T,
}

impl<T: Trace> GcCell<T> {
    #[inline]
    pub(crate) fn erase(ptr: *mut Self) -> *mut GcCell<Data> {
        ptr as _
    }

    pub(crate) unsafe fn data(this: *mut Self) -> *mut T {
        addr_of_mut!((*this).data)
    }
}

impl GcCell<Data> {
    pub(crate) unsafe fn free(this: *mut Self) {
        let vt = (*this).header.vt;
        let data = addr_of_mut!((*this).data);
        let drop_in_place = addr_of!((*vt).drop_in_place).read();
        let size = addr_of!((*vt).size).read();
        let align = addr_of!((*vt).align).read();
        let layout = Layout::new::<GcHeader>()
            .extend(Layout::from_size_align(size, align).unwrap())
            .unwrap()
            .0
            .pad_to_align();

        debug!("free {this:p} {layout:?}");

        drop_in_place(data);
        dealloc(this as *mut u8, layout)
    }

    #[inline]
    pub(crate) unsafe fn trace(this: *const Self) {
        if Self::is_marked(this) {
            debug!("already marked {:p}", this);
            return;
        }

        debug!("trace {:p}", this);
        Self::set_mark(this, true);

        let vt = (*this).header.vt;
        let data = addr_of!((*this).data) as *const Data;

        {
            let trace = addr_of!((*vt).trace).read();
            trace(data);
        }
    }

    #[inline]
    pub(crate) unsafe fn set_mark(this: *const Self, v: bool) {
        (*this).header.mark.get().write(v);
    }

    #[inline]
    pub(crate) unsafe fn is_marked(this: *const Self) -> bool {
        (*this).header.mark.get().read()
    }

    #[inline]
    pub(crate) unsafe fn set_prev(this: *const Self, ptr: *mut GcCell<Data>) {
        (*this).header.prev.get().write(ptr);
    }

    #[inline]
    pub(crate) unsafe fn get_prev(this: *const Self) -> *mut GcCell<Data> {
        (*this).header.prev.get().read()
    }
}

struct GcHeader {
    prev: UnsafeCell<*mut GcCell<Data>>,
    vt: *mut Vt,
    mark: UnsafeCell<bool>,
}

pub type Data = ();

#[repr(C)]
struct Vt {
    drop_in_place: unsafe fn(*mut Data),
    size: usize,
    align: usize,
    trace: fn(*const Data),
}

impl Vt {
    #[inline]
    const fn get<T: Trace>() -> *mut Vt {
        // miri won't let us directly access the vtable, but we're willing
        // to accept any risk about its unstable layout.
        // so when running in miri, we use a manually constructed vtable.

        {
            trait HasVt<T: ?Sized> {
                const VT: &'static Vt;
            }

            impl<T: Trace> HasVt<T> for T {
                const VT: &'static Vt = unsafe {
                    use std::mem::align_of;
                    use std::mem::size_of;
                    use std::ptr::drop_in_place;

                    &Vt {
                        drop_in_place: transmute::<unsafe fn(*mut T), unsafe fn(*mut Data)>(
                            drop_in_place::<T> as unsafe fn(*mut T),
                        ),
                        size: size_of::<T>(),
                        align: align_of::<T>(),
                        trace: transmute::<unsafe fn(&T), fn(*const Data)>(<T as Trace>::trace),
                    }
                };
            }

            <T as HasVt<T>>::VT as *const _ as *mut _
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering;

    #[test]
    fn alloc_dealloc() {
        struct Test {
            value: String,
        }

        unsafe impl Trace for Test {
            unsafe fn trace(&self) {}
        }

        let cx = Allocator::new(Config::default());
        let v = Test {
            value: "test".to_owned(),
        };
        let v = cx.alloc(v);
        unsafe {
            println!("{}", (*v).data.value);
        }

        unsafe { GcCell::free(GcCell::erase(v)) }
    }

    #[test]
    fn with_trace() {
        static TRACED: AtomicBool = AtomicBool::new(false);

        struct Test {}
        unsafe impl Trace for Test {
            unsafe fn trace(&self) {
                TRACED.store(true, Ordering::SeqCst);
            }
        }

        let cx = Allocator::new(Config::default());
        let v = cx.alloc(Test {});
        unsafe { GcCell::trace(GcCell::erase(v)) }

        assert!(TRACED.load(Ordering::SeqCst));

        unsafe { GcCell::free(GcCell::erase(v)) }
    }
}
