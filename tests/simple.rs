#[test]
fn it_adds() {
    assert_eq!(adder(), 2);
}

#[comptime::comptime]
fn adder() -> i32 {
    1 + 1
}
