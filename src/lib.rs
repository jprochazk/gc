#![allow(clippy::new_without_default)]

#[cfg(all(debug_assertions, __verbose_gc))]
fn type_name_of<T>(_: T) -> &'static str {
    std::any::type_name::<T>()
}

#[cfg(all(debug_assertions, __verbose_gc))]
macro_rules! __function {
    () => {{
        fn f() {}
        let mut name = $crate::type_name_of(f).strip_suffix("::f").unwrap();
        while let Some(v) = name.strip_suffix("::{{closure}}") {
            name = v;
        }
        name
    }};
}

macro_rules! debug {
    ($($tt:tt)+) => {
        #[cfg(all(debug_assertions, __verbose_gc))] {
            print!("[");
            print!("{}", __function!());
            print!("]: ");
            println!($($tt)+);
        }
    }
}

pub mod alloc;
pub mod gc;
pub mod handle;

pub(crate) fn default<T: Default>() -> T {
    T::default()
}
