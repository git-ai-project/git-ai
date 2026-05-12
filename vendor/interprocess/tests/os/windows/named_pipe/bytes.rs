use {
    super::drive_server,
    crate::{
        os::windows::named_pipe::{
            pipe_mode, DuplexPipeStream, PipeListener, RecvPipeStream, SendPipeStream,
        },
        tests::util::*,
    },
    std::{
        io::{prelude::*, BufReader},
        sync::{
            mpsc::{Receiver, Sender},
            Arc,
        },
    },
};

fn msg(server: bool) -> Box<str> { message(None, server, Some('\n')) }

fn handle_conn_duplex(
    listener: &mut PipeListener<pipe_mode::Bytes, pipe_mode::Bytes>,
) -> TestResult {
    let (mut recver, mut sender) = listener.accept().opname("accept")?.split();
    recv(&mut recver, msg(false))?;
    send(&mut sender, msg(true))?;
    DuplexPipeStream::reunite(recver, sender).opname("reunite")?;
    Ok(())
}
fn handle_conn_cts(listener: &mut PipeListener<pipe_mode::Bytes, pipe_mode::None>) -> TestResult {
    let mut recver = listener.accept().opname("accept")?;
    recv(&mut recver, msg(false))
}
fn handle_conn_stc(listener: &mut PipeListener<pipe_mode::None, pipe_mode::Bytes>) -> TestResult {
    let mut sender = listener.accept().opname("accept")?;
    send(&mut sender, msg(true))
}

pub fn server_duplex(
    id: &str,
    name_sender: Sender<Arc<str>>,
    num_clients: u32,
    doa_sync: Receiver<()>,
) -> TestResult {
    drive_server(
        id,
        name_sender,
        num_clients,
        |plo| plo.create_duplex::<pipe_mode::Bytes>(),
        handle_conn_duplex,
        doa_sync,
    )
}
pub fn server_cts(
    id: &str,
    name_sender: Sender<Arc<str>>,
    num_clients: u32,
    doa_sync: Receiver<()>,
) -> TestResult {
    drive_server(
        id,
        name_sender,
        num_clients,
        |plo| plo.create_recv_only::<pipe_mode::Bytes>(),
        handle_conn_cts,
        doa_sync,
    )
}
pub fn server_stc(
    id: &str,
    name_sender: Sender<Arc<str>>,
    num_clients: u32,
    doa_sync: Receiver<()>,
) -> TestResult {
    drive_server(
        id,
        name_sender,
        num_clients,
        |plo| plo.create_send_only::<pipe_mode::Bytes>(),
        handle_conn_stc,
        doa_sync,
    )
}

pub fn client_duplex(name: &str, doa: bool) -> TestResult {
    let conn = DuplexPipeStream::<pipe_mode::Bytes>::connect_by_path(name).opname("connect")?;
    if doa {
        return Ok(());
    }
    let (mut recver, mut sender) = conn.split();
    send(&mut sender, msg(false))?;
    recv(&mut recver, msg(true))?;
    DuplexPipeStream::reunite(recver, sender).opname("reunite")?;
    Ok(())
}
pub fn client_cts(name: &str, doa: bool) -> TestResult {
    let mut sender =
        SendPipeStream::<pipe_mode::Bytes>::connect_by_path(name).opname("connect")?;
    if doa {
        return Ok(());
    }
    send(&mut sender, msg(false))
}
pub fn client_stc(name: &str, doa: bool) -> TestResult {
    let mut recver =
        RecvPipeStream::<pipe_mode::Bytes>::connect_by_path(name).opname("connect")?;
    if doa {
        return Ok(());
    }
    recv(&mut recver, msg(true))
}

fn recv(conn: &mut RecvPipeStream<pipe_mode::Bytes>, exp: impl AsRef<str>) -> TestResult {
    let mut conn = BufReader::new(conn);
    let exp_ = exp.as_ref();
    let mut buf = String::with_capacity(exp_.len());
    conn.read_line(&mut buf).opname("receive")?;
    ensure_eq!(buf, exp_);
    Ok(())
}
fn send(conn: &mut SendPipeStream<pipe_mode::Bytes>, msg: impl AsRef<str>) -> TestResult {
    conn.write_all(msg.as_ref().as_bytes()).opname("send")?;
    conn.flush().opname("flush")
}
