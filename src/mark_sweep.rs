use crate::Object;
use std::any::TypeId;
use std::cell::Cell;
use std::ptr::NonNull;

#[doc(hidden)]
struct Vtable<T: ?Sized> {
  drop_in_place: unsafe fn(*mut T),
  display_fmt: fn(*const T, &mut std::fmt::Formatter<'_>) -> std::fmt::Result,
  debug_fmt: fn(*const T, &mut std::fmt::Formatter<'_>) -> core::fmt::Result,
}

trait HasVtable<T> {
  const VTABLE: *const Vtable<T>;
}
impl<T: Object> HasVtable<T> for T {
  const VTABLE: *const Vtable<T> = &Vtable {
    drop_in_place: std::ptr::drop_in_place::<T>,
    display_fmt: |p, f| {
      // Safety:
      // `p` is guaranteed to be non-null and valid for reads.
      // See `NonNull` in `Any` and `Ref<T>`
      <T as core::fmt::Display>::fmt(unsafe { p.as_ref().unwrap_unchecked() }, f)
    },
    debug_fmt: |p, f| {
      // Safety:
      // `p` is guaranteed to be non-null and valid for reads.
      // See `NonNull` in `Any` and `Ref<T>`
      <T as core::fmt::Debug>::fmt(unsafe { p.as_ref().unwrap_unchecked() }, f)
    },
  };
}

struct GcBox<T: ?Sized> {
  next: Cell<Option<NonNull<GcBox<()>>>>,
  type_id: TypeId,
  vtable: *const Vtable<T>,
  data: T,
}

pub struct Gc {
  heap_size: Cell<usize>,
  threshold: Cell<usize>,
  max_heap_size: Cell<usize>,
  roots: Vec<NonNull<GcBox<()>>>,
}

impl Gc {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn with_capacity(capacity: usize) -> Self {
    Self {
      heap_size: Cell::new(0),
      threshold: Cell::new(3 * capacity / 4),
      max_heap_size: Cell::new(capacity),
      roots: Vec::new(),
    }
  }
}

impl Default for Gc {
  fn default() -> Self {
    Self::with_capacity(4096)
  }
}
