use rstest::*;

#[fixture]
async fn a() -> u32 {
    1
}

#[rstest]
#[test_attr(macro_rules_attribute::apply(smol_macros::test!))]
#[awt]
#[case(3)]
#[case(2341)]
async fn async_test(#[future] a: u32, #[case] b: u32, #[values(2, 4, 16)] c: u32) {
    assert_eq!((a * b * c) % 2, 0);
}
