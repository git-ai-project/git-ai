//{
#[cfg(not(all(windows, feature = "tokio")))]
fn main() {
    #[rustfmt::skip] eprintln!("\
This example is not available on platforms other than Windows or when the \
Tokio feature is disabled.");
}
#[cfg(all(windows, feature = "tokio"))]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    //}
    use {
        interprocess::os::windows::named_pipe::{pipe_mode, tokio::*},
        tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            try_join,
        },
    };

    let name = r"\\.\pipe\Example";
    let mut buffer = String::with_capacity(128);

    let conn = DuplexPipeStream::<pipe_mode::Bytes>::connect_by_path(name).await?;
    let [mut recver, mut sender] = [&conn; 2];

    let send = async {
        sender.write_all(b"Hello from client!").await?;
        // Make sure Tokio flushes its internal buffer.
        sender.shutdown().await?;
        Ok(())
    };
    let recv = recver.read_to_string(&mut buffer);

    try_join!(send, recv)?;

    // Avoid holding up resources.
    drop(conn);

    println!("Server answered: {buffer}");
    //{
    Ok(())
} //}
