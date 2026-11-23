pub struct Engine;

pub fn compute(value: u32) -> u32 {
    value + 1
}

pub fn run() -> u32 {
    let first = compute(1);
    let alias = compute;
    first + alias(2)
}

pub fn dispatch(callback: fn(u32) -> u32) -> u32 {
    callback(3)
}
