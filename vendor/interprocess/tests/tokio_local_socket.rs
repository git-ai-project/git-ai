// TODO test various error conditions

mod no_server;
mod stream;
mod off_runtime_drop;

use crate::tests::util::{self, tokio::test_wrapper, TestResult};

#[allow(clippy::type_complexity)]
async fn test_stream(id: &'static str, path: bool) -> TestResult {
    use stream::*;
    util::tokio::drive_server_and_multiple_clients(
        move |s, n| server(id, handle_client, s, n, path),
        client,
    )
    .await
}

macro_rules! matrix {
    ($($nm:ident $path:ident)+) => {$(
        #[test]
        fn $nm() -> TestResult { test_wrapper(test_stream(make_id!(), $path)) }
    )+};
}

matrix! {
    stream_file       true
    stream_namespaced false
}

#[test]
fn no_server_file() -> TestResult { test_wrapper(no_server::run_and_verify_error(true)) }
#[test]
fn no_server_namespaced() -> TestResult { test_wrapper(no_server::run_and_verify_error(false)) }
