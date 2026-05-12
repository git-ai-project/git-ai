#[allow(dead_code)]
pub(crate) fn doc() {
    use crate::Num;

    // ANCHOR: impl
    struct One; //
    impl Num for One {
        fn num(&self) -> u8 { 1 }
    }

    struct Two; //
    impl Num for Two {
        fn num(&self) -> u8 { 2 }
    }
    // ANCHOR_END: impl

    // ANCHOR: one_is_1
    assert!(One.num() == 1);
    // ANCHOR_END: one_is_1

    // ANCHOR: impls_differ
    assert_ne!(One.num(), One.num());
    // ANCHOR_END: impls_differ
}
