use {
    crate::{
        local_socket::{
            tokio::{prelude::*, Stream},
            ListenerOptions, Name,
        },
        tests::util::*,
        BoolExt, SubUsizeExt,
    },
    ::tokio::{
        io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader},
        sync::oneshot::Sender,
        task, try_join,
    },
    color_eyre::eyre::WrapErr,
    std::{future::Future, str, sync::Arc},
};

fn msg(server: bool, nts: bool) -> Box<str> {
    message(None, server, Some(['\n', '\0'][nts.to_usize()]))
}

pub async fn server<HCF: Future<Output = TestResult> + Send + 'static>(
    id: &str,
    mut handle_client: impl FnMut(Stream) -> HCF,
    name_sender: Sender<Arc<Name<'static>>>,
    num_clients: u32,
    path: bool,
) -> TestResult {
    let (name, listener) = listen_and_pick_name(&mut namegen_local_socket(id, path), |nm| {
        ListenerOptions::new().name(nm.borrow()).create_tokio()
    })?;

    let _ = name_sender.send(Arc::new(name));

    let mut tasks = Vec::with_capacity(num_clients.try_into().unwrap());
    for _ in 0..num_clients {
        let conn = listener.accept().await.opname("accept")?;
        tasks.push(task::spawn(handle_client(conn)));
    }
    for task in tasks {
        task.await
            .context("server task panicked")?
            .context("server task returned early with error")?;
    }
    Ok(())
}

pub async fn handle_client(conn: Stream) -> TestResult {
    let (mut recver, mut sender) = (BufReader::new(&conn), &conn);
    let recv = async {
        recv(&mut recver, &msg(false, false), 0).await?;
        recv(&mut recver, &msg(false, true), 1).await
    };
    let send = async {
        send(&mut sender, &msg(true, false), 0).await?;
        send(&mut sender, &msg(true, true), 1).await
    };
    try_join!(recv, send).map(|((), ())| ())
}

pub async fn client(nm: Arc<Name<'static>>) -> TestResult {
    let conn = Stream::connect(nm.borrow()).await.opname("connect")?;
    let (mut recver, mut sender) = (BufReader::new(&conn), &conn);
    let recv = async {
        recv(&mut recver, &msg(true, false), 0).await?;
        recv(&mut recver, &msg(true, true), 1).await
    };
    let send = async {
        send(&mut sender, &msg(false, false), 0).await?;
        send(&mut sender, &msg(false, true), 1).await
    };
    try_join!(recv, send).map(|((), ())| ())
}

async fn recv(conn: &mut (dyn AsyncBufRead + Unpin + Send), exp: &str, nr: u8) -> TestResult {
    let term = *exp.as_bytes().last().unwrap();
    let fs = ["first", "second"][nr.to_usize()];

    let mut buffer = Vec::with_capacity(exp.len());
    conn.read_until(term, &mut buffer)
        .await
        .wrap_err_with(|| format!("{} receive failed", fs))?;
    ensure_eq!(
        str::from_utf8(&buffer).with_context(|| format!("{} receive wasn't valid UTF-8", fs))?,
        exp,
    );
    Ok(())
}

async fn send(conn: &mut (dyn AsyncWrite + Unpin + Send), msg: &str, nr: u8) -> TestResult {
    let fs = ["first", "second"][nr.to_usize()];
    conn.write_all(msg.as_bytes()).await.with_context(|| format!("{} socket send failed", fs))
}
