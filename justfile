set windows-shell := ["pwsh.exe", "-NoLogo", "-Command"]

test:
  RUSTFLAGS="--cfg=__verbose_gc" cargo test --all-features

miri:
  cargo miri test --all-features

miri-slow:
  RUSTFLAGS="--cfg=__verbose_gc" cargo miri test --all-features

check:
  RUSTFLAGS="--cfg=__verbose_gc" cargo check --all-features
