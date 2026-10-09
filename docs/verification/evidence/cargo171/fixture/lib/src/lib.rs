pub fn value() -> u32 { mac::answer!() + 1 }
pub fn here() -> &'static str { file!() }
#[cfg(test)] mod t { #[test] fn v() { assert_eq!(super::value(), 43); } }
