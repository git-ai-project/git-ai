use std::future::Future;

use crate::error::GitAiError;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to create Tokio runtime")
}

pub fn block_on<F>(future: F) -> F::Output
where
    F: Future + Send,
    F::Output: Send,
{
    if tokio::runtime::Handle::try_current().is_ok() {
        std::thread::scope(|scope| {
            scope
                .spawn(move || runtime().block_on(future))
                .join()
                .expect("Tokio helper thread panicked")
        })
    } else {
        runtime().block_on(future)
    }
}

pub async fn spawn_blocking_result<F, T>(task: F) -> Result<T, GitAiError>
where
    F: FnOnce() -> Result<T, GitAiError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(task)
        .await
        .map_err(|err| GitAiError::Generic(format!("Tokio blocking task failed: {err}")))?
}
