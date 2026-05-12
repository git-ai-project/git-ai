//{
#[cfg(not(windows))]
fn main() {
    eprintln!("This example is not available on platforms other than Windows.");
}
#[cfg(windows)]
fn main() -> std::io::Result<()> {
    //}
    use {interprocess::os::windows::named_pipe::*, recvmsg::prelude::*};

    let name = r"\\.\pipe\Example";

    // The right size depends on the specifics of the protocol in use.
    let mut buffer = MsgBuf::from(Vec::with_capacity(128));

    // Will fail immediately if the server hasn't started yet.
    let mut conn = DuplexPipeStream::<pipe_mode::Messages>::connect_by_path(name)?;

    // Here's our message so that we can check the length of sent data.
    const MESSAGE: &[u8] = b"Hello from client!";
    // Send the message, getting the amount of bytes that was actually sent in return.
    let sent = conn.send(MESSAGE)?;
    // If the length doesn't match, something is seriously wrong.
    assert_eq!(sent, MESSAGE.len());

    // Use the reliable message receive API, which gets us a RecvResult
    // from the recvmsg crate.
    conn.recv_msg(&mut buffer, None)?;

    // Avoid holding up resources.
    drop(conn);

    // Convert the data that's been received into a string. This checks for
    // UTF-8 validity, and if invalid characters are found, a new buffer
    // is allocated to house a modified version of the received data, where
    // decoding errors are replaced with those diamond-shaped question mark
    // U+FFFD REPLACEMENT CHARACTER thingies: �.
    let received_string = String::from_utf8_lossy(buffer.filled_part());

    println!("Server answered: {received_string}");
    //{
    Ok(())
} //}
