use std::cell::UnsafeCell;
use std::marker::PhantomData;
use std::ops::Deref;
use std::ptr;

#[repr(C)]
pub struct Root<'cx, T: Rootable> {
  base: RootBase,
  // TODO: this should be `Value` or `*mut T` where `T: Object`
  ptr: T::Pointer,

  // Ensure that both the lifetime `'cx` and the type `T` are invariant
  lifetime: PhantomData<fn(&'cx ()) -> &'cx ()>,
  invariant: PhantomData<&'cx mut T>,
}

impl<'cx, P, T> Root<'cx, T>
where
  P: RootablePointer<Pointee = T>,
  T: Rootable<Pointer = P>,
{
  #[inline]
  pub fn new(ptr: P) -> Self {
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
pub struct Rooted<'root, 'cx, T: Rootable> {
  root: *mut Root<'cx, T>,
  lifetime: PhantomData<&'root mut ()>,
}

impl<'root, 'cx, T: Rootable> Rooted<'root, 'cx, T>
where
  'root: 'cx,
{
  #[inline]
  pub fn handle<'guard>(&'guard self) -> Handle<'guard, 'root, 'cx, T> {
    Handle { guard: self }
  }

  #[inline]
  pub fn handle_mut<'guard>(&'guard mut self) -> HandleMut<'guard, 'root, 'cx, T> {
    HandleMut { guard: self }
  }

  #[inline]
  pub fn raw(&self) -> T::Pointer {
    unsafe { (*self.root).ptr }
  }
}

impl<'root, 'cx, T: Rootable> Drop for Rooted<'root, 'cx, T> {
  fn drop(&mut self) {
    let head: *mut *mut RootBase = unsafe { ptr::addr_of_mut!((*self.root).base.head).read() };

    debug_assert!(
      ptr::eq(unsafe { head.read() }, self.root.cast::<RootBase>()),
      "Rooted dropped, but not as root list head"
    );

    unsafe {
      let prev = ptr::addr_of_mut!((*self.root).base.prev).read();
      head.write(prev);
    }
  }
}

#[derive(Clone, Copy)]
pub struct Handle<'guard, 'root, 'cx, T: Rootable> {
  guard: &'guard Rooted<'root, 'cx, T>,
}

impl<'guard, 'root, 'cx, T: Rootable> Handle<'guard, 'root, 'cx, T> {
  #[inline]
  pub fn get(&self) -> T
  where
    T: Copy,
  {
    unsafe { *T::get_ref(&(*self.guard.root).ptr) }
  }
}

impl<'guard, 'root, 'cx, T: Rootable> Deref for Handle<'guard, 'root, 'cx, T> {
  type Target = T;

  #[inline]
  fn deref(&self) -> &Self::Target {
    self.as_ref()
  }
}

impl<'guard, 'root, 'cx, T: Rootable> AsRef<T> for Handle<'guard, 'root, 'cx, T> {
  #[inline]
  fn as_ref(&self) -> &T {
    unsafe { T::get_ref(&(*self.guard.root).ptr) }
  }
}

pub struct HandleMut<'guard, 'root, 'cx, T: Rootable> {
  guard: &'guard mut Rooted<'root, 'cx, T>,
}

impl<'guard, 'root, 'cx, T: Rootable> HandleMut<'guard, 'root, 'cx, T> {
  #[inline]
  pub fn get(&self) -> T
  where
    T: Copy,
  {
    unsafe { *T::get_ref(&(*self.guard.root).ptr) }
  }

  #[inline]
  pub fn set(&mut self, ptr: T::Pointer) {
    unsafe { (*self.guard.root).ptr = ptr }
  }
}

impl<'guard, 'root, 'cx, T: Rootable> Deref for HandleMut<'guard, 'root, 'cx, T> {
  type Target = T;

  #[inline]
  fn deref(&self) -> &Self::Target {
    self.as_ref()
  }
}

impl<'guard, 'root, 'cx, T: Rootable> AsRef<T> for HandleMut<'guard, 'root, 'cx, T> {
  #[inline]
  fn as_ref(&self) -> &T {
    unsafe { T::get_ref(&(*self.guard.root).ptr) }
  }
}

#[doc(hidden)]
#[macro_export]
macro_rules! __root {
  (in $cx:ident; $binding:ident = $ptr:expr) => {
    let mut __root = $crate::rooting::Root::new($ptr);
    // SAFETY: `root` is held on the stack and will never move
    #[allow(unused_mut)]
    let mut $binding = unsafe { $cx.append(&mut __root) };
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
  Value,
  Object,
}

enum_array_index!(RootKind, RootKind::Object);

pub trait Rootable: private::Sealed {
  /// The root kind of `Self`, which determines how the GC will mark this root.
  const KIND: RootKind;

  /// The pointer type used to refer to `Self`.
  type Pointer: Copy;

  /// Get a reference to `v`.
  fn get_ref(v: &Self::Pointer) -> &Self;

  /// Get a raw pointer to `v` without creating a temporary reference.
  ///
  /// # Safety
  /// - `v` must not be null
  /// - The implementation must not create a temporary reference to `Self`
  unsafe fn get_raw(v: *mut Self::Pointer) -> *mut Self;
}

/// This is a helper trait used in `Root::new` to allow `Rust`
/// to infer the `T` in `Root<'_, T>` from a pointer to `T`.
pub trait RootablePointer: Copy + private::Sealed {
  /// The type `Self` points to.
  type Pointee: Rootable;
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
  pub unsafe fn append<'root, 'cx, T: Rootable>(
    &'cx self,
    root: &'root mut Root<'cx, T>,
  ) -> Rooted<'root, 'cx, T> {
    let head = self.head(T::KIND);

    let prev = *head;
    let head = head as *mut _;

    root.base.head = head;
    root.base.prev = prev;
    head.write(root as *mut _ as *mut RootBase);

    Rooted {
      root,
      lifetime: PhantomData,
    }
  }

  pub fn for_each_root<T, F>(&self, f: F)
  where
    T: 'static,
    T: Rootable,
    F: Fn(*mut T),
  {
    unsafe {
      let head = self.head(T::KIND);
      let mut current = *head;
      while !current.is_null() {
        let prev = ptr::addr_of!((*current).prev).read();
        let ptr = current.cast::<Root<'static, T>>();
        let ptr = T::get_raw(ptr::addr_of_mut!((*ptr).ptr));
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
    data: RefCell<Vec<(DropInPlace, Layout, *mut ())>>,
  }

  impl Drop for Arena {
    fn drop(&mut self) {
      for (drop_in_place, layout, v) in self.data.get_mut().iter().copied() {
        unsafe { drop_in_place(v) }
        unsafe { std::alloc::dealloc(v.cast::<u8>(), layout) };
      }
    }
  }

  impl Arena {
    fn new() -> Self {
      Self {
        data: RefCell::new(Vec::new()),
      }
    }

    fn alloc<T>(&mut self, v: T) -> *mut T {
      fn drop_in_place_for<T>() -> DropInPlace {
        let drop_in_place: unsafe fn(*mut T) = ptr::drop_in_place::<T>;
        let drop_in_place: DropInPlace = unsafe { transmute(drop_in_place) };
        drop_in_place
      }

      let v = Box::into_raw(Box::new(v));

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
    const KIND: RootKind = RootKind::Object;

    type Pointer = *mut Test;

    #[inline]
    fn get_ref(v: &Self::Pointer) -> &Self {
      unsafe { &**v }
    }

    #[inline]
    unsafe fn get_raw(v: *mut Self::Pointer) -> *mut Self {
      ptr::addr_of_mut!(**v)
    }
  }
  impl private::Sealed for *mut Test {}
  impl RootablePointer for *mut Test {
    type Pointee = Test;
  }

  #[test]
  fn rooting() {
    let mut arena = Arena::new();
    let cx = Context::new();

    // TODO: `root` macro should handle allocation
    let a = arena.alloc(Test { v: 100 });
    root!(in cx; a = a);

    let b = arena.alloc(Test { v: 50 });
    root!(in cx; b = b);

    assert_eq!(a.handle().v, 100);
    assert_eq!(b.handle().v, 50);
    assert!(!unsafe { cx.head(Test::KIND) }.is_null());

    // NOTE: at this point, `b` will be unrooted
    // this is fine because it's no longer reachable
    b.handle_mut().set(a.raw());

    assert_eq!(a.handle().v, 100);
    assert_eq!(b.handle().v, 100);
    assert!(!unsafe { cx.head(Test::KIND) }.is_null());

    let n = Cell::new(0usize);
    cx.for_each_root(|ptr: *mut Test| {
      n.set(n.get() + 1);

      let ptr = unsafe { &*ptr };
      assert_eq!(ptr.v, 100);
    });
    assert_eq!(n.get(), 2);
  }
}
