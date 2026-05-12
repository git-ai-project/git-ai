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
        interprocess::os::windows::named_pipe::{pipe_mode, tokio::*, PipeListenerOptions},
        std::{io, path::Path},
        tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            try_join,
        },
    };

    async fn handle_conn(conn: DuplexPipeStream<pipe_mode::Bytes>) -> io::Result<()> {
        let [mut recver, mut sender] = [&conn; 2];
        // Allocate a small buffer for receiving.
        let mut buffer = String::with_capacity(128);

        let send = async {
            sender.write_all(b"Hello from server!").await?;
            // Make sure Tokio flushes its internal buffer.
            sender.shutdown().await?;
            Ok(())
        };
        let recv = recver.read_to_string(&mut buffer);

        try_join!(recv, send)?;

        // Avoid holding up resources.
        drop(conn);

        // read_line keeps the line feed at the end.
        println!("Client answered: {buffer}");
        Ok(())
    }

    let pipe_name = r"\\.\pipe\Example";

    let listener = PipeListenerOptions::new()
        .path(Path::new(pipe_name))
        .create_tokio_duplex::<pipe_mode::Bytes>()?;

    // This is a good place to inform clients that the server is ready.
    eprintln!(r"Server running at {pipe_name}");

    loop {
        let conn = match listener.accept().await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("There was an error with an incoming connection: {e}");
                continue;
            }
        };

        // Spawn new parallel asynchronous tasks onto the Tokio runtime and
        // hand the connection over to them so that multiple clients could be
        // processed simultaneously in a lightweight fashion.
        tokio::spawn(async move {
            // The outer match processes errors that happen when we're
            // connecting to something. The inner if-let processes errors
            // that happen during the connection.
            if let Err(e) = handle_conn(conn).await {
                eprintln!("error while handling connection: {e}");
            }
        });
    }
} //
