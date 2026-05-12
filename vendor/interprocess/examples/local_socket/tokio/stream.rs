//{
#[cfg(not(feature = "tokio"))]
fn main() {
    eprintln!("This example is not available when the Tokio feature is disabled.");
}
#[cfg(feature = "tokio")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    //}
    use {
        interprocess::local_socket::{
            tokio::{prelude::*, Stream},
            GenericFilePath, GenericNamespaced,
        },
        tokio::{
            io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
            try_join,
        },
    };

    let name = if GenericNamespaced::is_supported() {
        "example.sock".to_ns_name::<GenericNamespaced>()?
    } else {
        "/tmp/example.sock".to_fs_name::<GenericFilePath>()?
    };

    let mut buffer = String::with_capacity(128);

    let conn = Stream::connect(name).await?;

    // Create a buffered reader that wraps the connection by reference
    // so we can receive a single line.
    let mut recver = BufReader::new(&conn);
    // The "sender" will just be a shared reference to the connection,
    // allowing us to read and write concurrently. It is okay to ditch
    // this reference and reborrow it at any time to satisfy the borrow
    // check.
    let mut sender = &conn;

    let send = sender.write_all(b"Hello from client!\n");
    let recv = recver.read_line(&mut buffer);

    try_join!(send, recv)?;

    // Avoid holding up resources.
    drop(recver);
    drop(conn);

    // read_line keeps the line feed at the end.
    print!("Server answered: {buffer}");
    //{
    Ok(())
} //}
