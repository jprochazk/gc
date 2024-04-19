set windows-shell := ["pwsh.exe", "-NoLogo", "-Command"]

test:
  RUSTFLAGS="--cfg=__verbose_gc" cargo test --all-features

miri:
  RUSTFLAGS="--cfg=__verbose_gc" cargo miri test --all-features
