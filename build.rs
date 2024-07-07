fn main() {
    println!("cargo::rustc-check-cfg=cfg(__verbose_gc)");
}
