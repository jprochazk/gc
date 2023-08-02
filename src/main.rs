fn main() {
  let v = Box::new(10i32);

  println!("{:064b}", v.as_ref() as *const _ as usize);
}
