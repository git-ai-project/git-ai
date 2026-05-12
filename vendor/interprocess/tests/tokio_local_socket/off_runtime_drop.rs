use {
    crate::{
        local_socket::{tokio::prelude::*, ListenerOptions, Name},
        tests::util::{listen_and_pick_name, namegen_local_socket, TestResult},
    },
    std::convert::Infallible,
    tokio::{io::AsyncWriteExt as _, runtime::Builder, task},
};

async fn create_server(name_sender: std::sync::mpsc::Sender<Name<'static>>) -> TestResult {
    // Not trying path = true because this was a Windows-only bug.
    let (name, listener) =
        listen_and_pick_name(&mut namegen_local_socket(make_id!(), false), |nm| {
            ListenerOptions::new().name(nm.borrow()).create_tokio()
        })?;
    let _ = name_sender.send(name);
    task::spawn(send_loop(listener.accept().await?));
    Ok(())
}

async fn send_loop(mut st: LocalSocketStream) -> TestResult<Infallible> {
    st.write_all(b"Hello, world!").await?;
    tokio::sync::Notify::new().notified().await;
    unreachable!()
}

#[test]
fn main() -> TestResult {
    let _client = Builder::new_current_thread().enable_io().build()?.block_on(async {
        let (name_tx, name_rx) = std::sync::mpsc::channel();
        task::spawn(create_server(name_tx));
        // Yield so that the server has a chance to start.
        task::yield_now().await;

        let client = LocalSocketStream::connect(name_rx.recv()?.borrow()).await?;

        // Let the server accept the connection.
        task::yield_now().await;
        // create_server has completed and the receiver has been dropped.

        // Let the sender do its thing.
        task::yield_now().await;
        TestResult::<_>::Ok(client)
        // Returning now leaves the sender in a position where its destructor runs
        // as the Tokio runtime is shutting down.
    })?;
    Ok(())
}
