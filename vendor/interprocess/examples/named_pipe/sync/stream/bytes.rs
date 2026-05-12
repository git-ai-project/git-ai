//{
#[cfg(not(windows))]
fn main() {
    eprintln!("This example is not available on platforms other than Windows.");
}
#[cfg(windows)]
fn main() -> std::io::Result<()> {
    //}
    use {
        interprocess::os::windows::named_pipe::*,
        std::io::{prelude::*, BufReader},
    };

    let name = r"\\.\pipe\Example";
    let mut buffer = String::with_capacity(128);

    // Will fail immediately if the server hasn't started yet.
    let mut conn = BufReader::new(DuplexPipeStream::<pipe_mode::Bytes>::connect_by_path(name)?);

    // BufReader doesn't pass Write through, so we use get_mut.
    conn.get_mut().write_all(b"Hello from client!\n")?;

    // We now employ the buffer we allocated prior and receive a single line,
    // interpreting a newline character as an end-of-file (because named pipes
    // have no concept of partial shutdown), verifying validity of UTF-8 on
    // the fly.
    conn.read_line(&mut buffer)?;

    // Avoid holding up resources.
    drop(conn);

    // read_line keeps the line feed at the end.
    print!("Server answered: {buffer}");
    //{
    Ok(())
} //}
