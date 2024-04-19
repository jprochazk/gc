#![allow(clippy::new_without_default)]

macro_rules! function {
    () => {{
        fn f() {}
        fn type_name_of<T>(_: T) -> &'static str {
            std::any::type_name::<T>()
        }
        let name = type_name_of(f);
        name.strip_suffix("::f").unwrap()
    }};
}

macro_rules! debug {
    ($($tt:tt)+) => {
        #[cfg(all(debug_assertions, __verbose_gc))] {
            print!("[");
            print!("{}", function!());
            print!("]: ");
            println!($($tt)+);
        }
    }
}

pub mod alloc;
pub mod gc;
pub mod handle;
