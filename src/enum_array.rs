use std::marker::PhantomData;
use std::ops::{Deref, DerefMut, Index, IndexMut};

pub struct EnumArray<const LEN: usize, E: Into<usize>, V: Copy> {
  array: [V; LEN],
  index: PhantomData<fn() -> E>,
}

impl<const LEN: usize, E: Into<usize>, V: Copy> EnumArray<LEN, E, V> {
  pub fn new(v: V) -> Self {
    Self {
      array: [v; LEN],
      index: PhantomData,
    }
  }
}

/// # Safety
/// - `array` must be valid for reads and writes
pub unsafe fn get_raw<const LEN: usize, E: Into<usize>, V: Copy>(
  array: *mut EnumArray<LEN, E, V>,
  index: E,
) -> *mut V {
  let index: usize = index.into();
  std::ptr::addr_of_mut!((*array).array[index])
}

impl<const LEN: usize, E: Into<usize>, V: Copy> Deref for EnumArray<LEN, E, V> {
  type Target = [V; LEN];

  fn deref(&self) -> &Self::Target {
    &self.array
  }
}

impl<const LEN: usize, E: Into<usize>, V: Copy> DerefMut for EnumArray<LEN, E, V> {
  fn deref_mut(&mut self) -> &mut Self::Target {
    &mut self.array
  }
}

impl<const LEN: usize, E: Into<usize>, V: Copy> Index<E> for EnumArray<LEN, E, V> {
  type Output = V;

  fn index(&self, index: E) -> &Self::Output {
    self.array.index(index.into())
  }
}

impl<const LEN: usize, E: Into<usize>, V: Copy> IndexMut<E> for EnumArray<LEN, E, V> {
  fn index_mut(&mut self, index: E) -> &mut Self::Output {
    self.array.index_mut(index.into())
  }
}

pub trait Enum: Into<usize> {
  const LEN: usize;
}

macro_rules! enum_array_index {
  ($E:ty, $MAX:expr) => {
    impl From<$E> for usize {
      fn from(v: $E) -> usize {
        v as usize
      }
    }

    impl $crate::enum_array::Enum for $E {
      const LEN: usize = ($MAX as usize) + 1;
    }
  };
}

macro_rules! enum_array_type {
  ($vis:vis type $Name:ident = [$V:ty; $E:ty]) => {
    $vis type $Name =
      $crate::enum_array::EnumArray<{ <$E as $crate::enum_array::Enum>::LEN }, $E, $V>;
  };
}

pub trait FixedArray<V: Copy>: private::Sealed + Index<usize> + IndexMut<usize> {
  const LEN: usize;

  fn init(v: V) -> Self;
}

impl<V: Copy, const N: usize> private::Sealed for [V; N] {}
impl<V: Copy, const N: usize> FixedArray<V> for [V; N] {
  const LEN: usize = N;

  fn init(v: V) -> Self {
    [v; N]
  }
}

mod private {
  pub trait Sealed {}
}
