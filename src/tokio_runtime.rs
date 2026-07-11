use std::future::Future;
use std::sync::OnceLock;

use crate::error::GitAiError;

const HELPER_RUNTIME_WORKER_THREADS: usize = 2;
const HELPER_RUNTIME_MAX_BLOCKING_THREADS: usize = 4;

// Recreating a CPU-sized runtime here leaves one allocator arena per worker in
// long-lived daemon processes. Keep one small pool warm and reuse it instead.
fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(HELPER_RUNTIME_WORKER_THREADS)
            .max_blocking_threads(HELPER_RUNTIME_MAX_BLOCKING_THREADS)
            .thread_keep_alive(std::time::Duration::from_secs(60))
            .enable_all()
            .build()
            .expect("failed to create Tokio helper runtime")
    })
}

pub(crate) fn initialize() {
    let _ = runtime();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_block_on_uses_shared_helper_runtime() {
        let outer = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();

        outer.block_on(async {
            let outer_id = tokio::runtime::Handle::current().id();
            let (nested_id, nested_flavor) = block_on(async {
                let handle = tokio::runtime::Handle::current();
                (handle.id(), handle.runtime_flavor())
            });

            assert_ne!(nested_id, outer_id);
            assert_eq!(nested_flavor, tokio::runtime::RuntimeFlavor::MultiThread);
        });
    }

    #[test]
    fn repeated_block_on_reuses_helper_runtime() {
        let first_id = block_on(async { tokio::runtime::Handle::current().id() });
        let second_id = block_on(async { tokio::runtime::Handle::current().id() });

        assert_eq!(first_id, second_id);
    }

    #[test]
    fn helper_runtime_blocking_pool_is_bounded() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        block_on(async {
            let mut tasks = Vec::new();
            for _ in 0..=HELPER_RUNTIME_MAX_BLOCKING_THREADS {
                let active = Arc::clone(&active);
                let peak = Arc::clone(&peak);
                tasks.push(tokio::task::spawn_blocking(move || {
                    let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(current, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    active.fetch_sub(1, Ordering::SeqCst);
                }));
            }
            for task in tasks {
                task.await.unwrap();
            }
        });

        assert!(peak.load(Ordering::SeqCst) <= HELPER_RUNTIME_MAX_BLOCKING_THREADS);
    }
}
