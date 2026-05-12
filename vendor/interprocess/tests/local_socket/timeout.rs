use {
    crate::{
        local_socket::{prelude::*, ConnectOptions, ListenerOptions},
        tests::util::*,
        ConnectWaitMode,
    },
    color_eyre::eyre::bail,
    std::{
        fmt::Debug,
        io::{self, prelude::*},
        time::Duration,
    },
};

pub fn main(id: &str, path: bool) -> TestResult {
    let (nm, _listener) = listen_and_pick_name(&mut namegen_local_socket(id, path), |nm| {
        ListenerOptions::new().name(nm.borrow()).create_sync()
    })?;
    let mut conn = ConnectOptions::new()
        .name(nm)
        .wait_mode(ConnectWaitMode::Timeout(Duration::from_millis(1)))
        .connect_sync()
        .opname("connect")?;
    conn.set_recv_timeout(Some(Duration::from_micros(200))).opname("set_recv_timeout")?;
    conn.set_send_timeout(Some(Duration::from_micros(200))).opname("set_send_timeout")?;
    let mut buf = [0; 16];
    verify_timeout_error("read", conn.read_exact(&mut buf))?;
    conn.write_all(&buf).opname("write")?;
    Ok(())
}

fn verify_timeout_error<T: Debug>(opname: &str, r: io::Result<T>) -> TestResult {
    match r {
        Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
            Ok(())
        }
        Err(e) => Err(e).opname(opname),
        Ok(val) => bail!("{opname}: expected timeout, got {val:#?}"),
    }
}
