use {
    crate::{
        local_socket::{prelude::*, ListenerOptions, Name, Stream},
        tests::util::*,
        BoolExt, SubUsizeExt,
    },
    color_eyre::eyre::WrapErr,
    std::{
        io::{BufRead, BufReader, Write},
        str,
        sync::{mpsc::Sender, Arc},
    },
};

fn msg(server: bool, nts: bool) -> Box<str> {
    message(None, server, Some(['\n', '\0'][nts.to_usize()]))
}

fn fork(a: impl FnOnce() -> TestResult, b: impl FnOnce() -> TestResult + Send) -> TestResult {
    std::thread::scope(|scope| {
        let b = scope.spawn(b);
        a()?;
        b.join().unwrap()
    })
}

fn check_peer_creds(s: &Stream) -> TestResult {
    let creds = s.peer_creds().opname("peer_creds")?;
    #[allow(clippy::cast_sign_loss)]
    if let Some(pid) = creds.pid() {
        ensure_eq!(pid as u32, std::process::id());
    }
    #[cfg(unix)]
    {
        if let Some(uid) = creds.euid() {
            ensure_eq!(uid, unsafe { libc::geteuid() });
        }
        if let Some(gid) = creds.egid() {
            ensure_eq!(gid, unsafe { libc::getegid() });
        }
    }
    Ok(())
}

pub fn server(
    id: &str,
    handle_client: fn(Stream) -> TestResult,
    name_sender: Sender<Arc<Name<'static>>>,
    num_clients: u32,
    path: bool,
) -> TestResult {
    let (name, listener) = listen_and_pick_name(&mut namegen_local_socket(id, path), |nm| {
        ListenerOptions::new().name(nm.borrow()).create_sync()
    })?;
    let _ = name_sender.send(Arc::new(name));
    listener
        .incoming()
        .take(num_clients.try_into().unwrap())
        .try_for_each(|conn| handle_client(conn.opname("accept")?))
}

pub fn handle_client(conn: Stream) -> TestResult {
    check_peer_creds(&conn)?;
    conn.set_nonblocking(true).opname("set_nonblocking(true)")?;
    conn.set_nonblocking(false).opname("set_nonblocking(false)")?;
    let mut rx = BufReader::new(&conn);
    let mut tx = &conn;
    fork(|| recv(&mut rx, &msg(false, false), 0), || send(&mut tx, &msg(true, false), 0))?;
    fork(|| recv(&mut rx, &msg(false, true), 1), || send(&mut tx, &msg(true, true), 1))?;
    Ok(())
}

pub fn client(name: &Name<'_>) -> TestResult {
    let conn = Stream::connect(name.borrow()).opname("connect")?;
    check_peer_creds(&conn)?;
    let mut rx = BufReader::new(&conn);
    let mut tx = &conn;
    conn.set_nonblocking(true).opname("set_nonblocking(true)")?;
    conn.set_nonblocking(false).opname("set_nonblocking(false)")?;
    fork(|| recv(&mut rx, &msg(true, false), 0), || send(&mut tx, &msg(false, false), 0))?;
    fork(|| send(&mut tx, &msg(false, true), 1), || recv(&mut rx, &msg(true, true), 1))?;
    Ok(())
}

fn recv(conn: &mut dyn BufRead, exp: &str, nr: u8) -> TestResult {
    let term = *exp.as_bytes().last().unwrap();
    let fs = ["first", "second"][nr.to_usize()];

    let mut buffer = Vec::with_capacity(exp.len());
    conn.read_until(term, &mut buffer).wrap_err_with(|| format!("{} receive failed", fs))?;
    ensure_eq!(
        str::from_utf8(&buffer).with_context(|| format!("{} receive wasn't valid UTF-8", fs))?,
        exp,
    );
    Ok(())
}
fn send(conn: &mut dyn Write, msg: &str, nr: u8) -> TestResult {
    let fs = ["first", "second"][nr.to_usize()];
    conn.write_all(msg.as_bytes()).with_context(|| format!("{} socket send failed", fs))
}
