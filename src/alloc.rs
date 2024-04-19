use crate::gc::Trace;
use std::alloc::dealloc;
use std::alloc::Layout;
use std::cell::Cell;
use std::cell::UnsafeCell;
use std::mem::align_of;
use std::mem::size_of;
use std::mem::transmute;
use std::ptr::addr_of;
use std::ptr::addr_of_mut;
use std::ptr::drop_in_place;
use std::ptr::null_mut;

pub struct Allocator {
    pub(crate) head: Cell<*mut GcCell<Data>>,
}

impl Allocator {
    #[inline]
    pub fn new() -> Self {
        Self {
            head: Cell::new(null_mut()),
        }
    }

    #[inline]
    pub fn alloc<T: Trace>(&self, data: T) -> *mut GcCell<T> {
        let ptr = Box::into_raw(Box::new(GcCell {
            header: GcHeader {
                prev: UnsafeCell::new(self.head.get()),
                vt: Vt::get(&data),
                mark: UnsafeCell::new(false),
            },
            data,
        }));
        debug!("alloc {ptr:p}");

        self.head.set(GcCell::erase(ptr));
        ptr
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
            debug!("marked {:p}", this);
            return;
        }

        debug!("trace {:p}", this);
        Self::set_mark(this, true);

        let vt = (*this).header.vt;
        let data = addr_of!((*this).data) as *const Data;

        #[cfg(not(miri))]
        {
            let object = transmute::<TraitObject, *const dyn Trace>(TraitObject { data, vt });
            (*object).trace();
        }

        #[cfg(miri)]
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

    #[cfg(miri)]
    trace: fn(*const Data),
}

impl Vt {
    #[inline]
    const fn get<T: Trace>(v: &T) -> *mut Vt {
        #[cfg(not(miri))]
        {
            let v = v as &dyn Trace;
            unsafe { transmute::<&dyn Trace, TraitObject>(v).vt }
        }

        #[cfg(miri)]
        {
            <T as HasVt<T>>::VT as *const _ as *mut _
        }
    }
}

trait HasVt<T: ?Sized> {
    const VT: &'static Vt;
}

impl<T: Trace> HasVt<T> for T {
    const VT: &'static Vt = unsafe {
        &Vt {
            drop_in_place: transmute::<unsafe fn(*mut T), unsafe fn(*mut Data)>(
                drop_in_place::<T> as unsafe fn(*mut T),
            ),
            size: size_of::<T>(),
            align: align_of::<T>(),
            #[cfg(miri)]
            trace: transmute::<fn(&T), fn(*const Data)>(<T as Trace>::trace),
        }
    };
}

#[repr(C)]
struct TraitObject {
    data: *const Data,
    vt: *mut Vt,
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

        impl Trace for Test {
            fn trace(&self) {}
        }

        let cx = Allocator::new();
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
        impl Trace for Test {
            fn trace(&self) {
                TRACED.store(true, Ordering::SeqCst);
            }
        }

        let cx = Allocator::new();
        let v = cx.alloc(Test {});
        unsafe { GcCell::trace(GcCell::erase(v)) }

        assert!(TRACED.load(Ordering::SeqCst));

        unsafe { GcCell::free(GcCell::erase(v)) }
    }
}
