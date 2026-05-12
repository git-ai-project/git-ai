use rstest::*;

use sqlx::SqlitePool;

fn sum(a: i32, b: i32, c: i32) -> i32 {
    a + b + c
}

#[rstest]
#[case(1, 2)]
#[case(2, 3)]
#[sqlx::test]
fn test_sum(#[case] a: i32, #[case] b: i32, #[values(1,2)] c: i32, #[ignore] pool: SqlitePool) {
    assert_eq!(sum(a, b, c), a + b + c);
}