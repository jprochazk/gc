#[macro_use]
pub mod enum_array;

pub mod rooting;

// pub mod mark_sweep;

use std::fmt::{Debug, Display};
use std::mem::needs_drop;

pub trait Object: Debug + Display {
  /// Whether or not `Self` needs to have its `Drop` impl called.
  ///
  /// This has a default implementation using `core::mem::needs_drop::<Self>`,
  /// which does the right thing in most cases, but it can be overridden in
  /// case the default is incorrect.
  ///
  /// For example, this always returns `false` for `List`, because we don't
  /// want the underlying `Vec` to drop its contents, as they will be GC'd
  /// automatically, and the GC is responsible for calling `Drop` impls.
  const NEEDS_DROP: bool = needs_drop::<Self>();
}
