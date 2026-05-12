//{
fn main() -> std::io::Result<()> {
    //}
    use {
        interprocess::local_socket::{prelude::*, GenericFilePath, GenericNamespaced, Stream},
        std::io::{prelude::*, BufReader},
    };

    let name = if GenericNamespaced::is_supported() {
        "example.sock".to_ns_name::<GenericNamespaced>()?
    } else {
        "/tmp/example.sock".to_fs_name::<GenericFilePath>()?
    };

    let mut buffer = String::with_capacity(128);

    // Will fail immediately if the server hasn't started yet.
    let mut conn = BufReader::new(Stream::connect(name)?);

    // BufReader doesn't pass Write through, so we use get_mut.
    conn.get_mut().write_all(b"Hello from client!\n")?;

    // We now employ the buffer we allocated prior and receive a single line,
    // interpreting a newline character as an end-of-file (because local
    // sockets cannot be portably shut down), verifying validity of UTF-8 on
    // the fly.
    conn.read_line(&mut buffer)?;

    // Avoid holding up resources.
    drop(conn);

    // read_line keeps the line feed at the end.
    print!("Server answered: {buffer}");
    //{
    Ok(())
} //}
