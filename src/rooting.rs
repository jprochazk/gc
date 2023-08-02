use std::cell::UnsafeCell;
use std::marker::PhantomData;
use std::ptr;
use std::ptr::NonNull;

#[repr(C)]
pub struct Root<'cx, T> {
  base: RootBase,
  ptr: NonNull<T>,

  // Ensure that both the lifetime `'cx` and the type `T` are invariant
  lifetime: PhantomData<fn(&'cx ()) -> &'cx ()>,
  invariant: PhantomData<&'cx mut T>,
}

impl<'cx, T: Rootable> Root<'cx, T> {
  pub fn new(ptr: NonNull<T>) -> Self {
    Self {
      base: RootBase {
        head: ptr::null_mut(),
        prev: ptr::null_mut(),
      },
      ptr,

      lifetime: PhantomData,
      invariant: PhantomData,
    }
  }
}

#[repr(C)]
pub struct RootGuard<'a, 'cx, T: Rootable> {
  root: *mut Root<'cx, T>,
  lifetime: PhantomData<&'a mut ()>,
}

impl<'a, 'cx, T: Rootable> RootGuard<'a, 'cx, T> {
  // TODO: This should return a `Handle<'self, T>`
  pub fn get(&self) -> &T {
    unsafe { (*self.root).ptr.as_ref() }
  }

  pub fn set(&mut self, ptr: NonNull<T>) {
    unsafe {
      (*self.root).ptr = ptr;
    }
  }
}

impl<'a, 'cx, T: Rootable> Drop for RootGuard<'a, 'cx, T> {
  fn drop(&mut self) {
    let head: *mut *mut RootBase = unsafe { ptr::addr_of_mut!((*self.root).base.head).read() };

    debug_assert!(
      ptr::eq(unsafe { head.read() }, self.root.cast::<RootBase>()),
      "RootGuard dropped, but not as root list head"
    );

    unsafe {
      let prev = ptr::addr_of_mut!((*self.root).base.prev).read();
      head.write(prev);
    }
  }
}

#[doc(hidden)]
#[macro_export]
macro_rules! __root {
  (in $cx:ident as $binding:ident; $ptr:expr) => {
    let mut __root = $crate::rooting::Root::new($ptr);
    // SAFETY: `root` is held on the stack and will never move
    let $binding = unsafe { $cx.append(&mut __root) };
  };
}

pub use crate::__root as root;
use crate::enum_array;

#[repr(C)]
pub struct RootBase {
  head: *mut *mut RootBase,
  prev: *mut RootBase,
}

// TODO: `RootKind` should be auto-generated from an object type list and `Rootable` implemented for each type.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RootKind {
  Test,
}

enum_array_index!(RootKind, RootKind::Test);

pub trait Rootable: private::Sealed {
  const KIND: RootKind;
}

mod private {
  pub trait Sealed {}
}

pub struct Context {
  // `Box` ensures `RootHeads` has a stable address
  heads: Box<UnsafeCell<RootHeads>>,
}

impl Context {
  #[allow(clippy::new_without_default)]
  pub fn new() -> Self {
    Self {
      heads: Box::new(UnsafeCell::new(RootHeads::new(ptr::null_mut()))),
    }
  }

  #[allow(clippy::mut_from_ref)]
  #[inline]
  unsafe fn head(&self, kind: RootKind) -> *mut *mut RootBase {
    enum_array::get_raw(self.heads.get(), kind)
  }

  /// # Safety:
  /// - `root` must not be moved after this call
  #[doc(hidden)]
  #[must_use]
  pub unsafe fn append<'a, 'cx, T: Rootable>(
    &'cx self,
    root: &'a mut Root<'cx, T>,
  ) -> RootGuard<'a, 'cx, T> {
    let head = self.head(T::KIND);

    let prev = *head;
    let head = head as *mut _;

    root.base.head = head;
    root.base.prev = prev;
    head.write(root as *mut _ as *mut RootBase);

    RootGuard {
      root,
      lifetime: PhantomData,
    }
  }

  pub fn for_each_root<T, F>(&self, f: F)
  where
    T: Rootable,
    F: Fn(NonNull<T>),
  {
    unsafe {
      let head = self.head(T::KIND);
      let mut current = *head;
      while !current.is_null() {
        let prev = ptr::addr_of!((*current).prev).read();
        let ptr = current.cast::<Root<'static, ()>>();
        let ptr = ptr::addr_of!((*ptr).ptr).read().cast::<T>();
        f(ptr);
        current = prev;
      }
    }
  }
}

enum_array_type!(pub type RootHeads = [*mut RootBase; RootKind]);

#[cfg(test)]
mod tests {
  use super::*;
  use std::alloc::Layout;
  use std::cell::{Cell, RefCell};
  use std::mem::transmute;

  type DropInPlace = unsafe fn(*mut ());

  struct Arena {
    data: RefCell<Vec<(DropInPlace, Layout, NonNull<()>)>>,
  }

  impl Drop for Arena {
    fn drop(&mut self) {
      for (drop_in_place, layout, v) in self.data.get_mut().iter().copied() {
        unsafe { drop_in_place(v.as_ptr()) }
        unsafe { std::alloc::dealloc(v.as_ptr().cast::<u8>(), layout) };
      }
    }
  }

  impl Arena {
    fn new() -> Self {
      Self {
        data: RefCell::new(Vec::new()),
      }
    }

    fn alloc<T>(&mut self, v: T) -> NonNull<T> {
      fn drop_in_place_for<T>() -> DropInPlace {
        let drop_in_place: unsafe fn(*mut T) = ptr::drop_in_place::<T>;
        let drop_in_place: DropInPlace = unsafe { transmute(drop_in_place) };
        drop_in_place
      }

      let v = unsafe { NonNull::new_unchecked(Box::into_raw(Box::new(v))) };

      self
        .data
        .borrow_mut()
        .push((drop_in_place_for::<T>(), Layout::new::<T>(), v.cast::<()>()));

      v
    }
  }

  struct Test {
    v: i32,
  }

  impl private::Sealed for Test {}
  impl Rootable for Test {
    const KIND: RootKind = RootKind::Test;
  }

  #[test]
  fn rooting() {
    let mut arena = Arena::new();
    let cx = Context::new();

    let a = arena.alloc(Test { v: 100 });
    root!(in cx as a; a);

    let b = arena.alloc(Test { v: 100 });
    root!(in cx as b; b);

    assert_eq!(a.get().v, 100);
    assert_eq!(b.get().v, 100);
    assert!(!unsafe { cx.head(Test::KIND) }.is_null());

    let n = Cell::new(0usize);
    cx.for_each_root(|ptr: NonNull<Test>| {
      n.set(n.get() + 1);

      let ptr = unsafe { ptr.as_ref() };
      assert_eq!(ptr.v, 100);
    });
    assert_eq!(n.get(), 2);
  }
}
