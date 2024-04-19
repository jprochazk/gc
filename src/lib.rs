#![allow(clippy::new_without_default)]

#[macro_use]
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

#[macro_use]
mod macros {
    macro_rules! debug {
        ($($tt:tt)+) => {
            #[cfg(all(debug_assertions, feature = "__verbose_gc"))] {
                use std::io::Write;
                let mut stderr = std::io::stderr();
                let _ = stderr.write_all("[".as_bytes());
                let _ = stderr.write_all(function!().as_bytes());
                let _ = stderr.write_all("]: ".as_bytes());
                let _ = writeln!(stderr, $($tt)+);
            }
        }
    }
}

pub mod alloc;
pub mod gc;
pub mod handle;
