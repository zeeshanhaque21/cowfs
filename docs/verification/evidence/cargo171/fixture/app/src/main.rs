include!(concat!(env!("OUT_DIR"), "/gen.rs"));
fn main() { println!("{} {} {}", lib::value(), G, lib::here()); }
