use crate::{AsyncRecvMsgExt, MsgBuf, RecvResult};
use std::{
    mem::MaybeUninit,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddrV4},
    str::from_utf8,
};
use tokio::{net::UdpSocket, try_join};

#[tokio::test]
async fn v4() {
    udp(false).await
}
#[tokio::test]
async fn v6() {
    udp(true).await
}

async fn udp(v6: bool) {
    let addr: IpAddr = if v6 { Ipv6Addr::LOCALHOST.into() } else { Ipv4Addr::LOCALHOST.into() };
    // The following two will choose different ports:
    let (mut s1, mut s2) =
        try_join!(UdpSocket::bind((addr, 0)), UdpSocket::bind((addr, 0))).expect("bind failed");

    let getport = |sock: &UdpSocket| sock.local_addr().expect("port query failed").port();
    let (p1, p2) = dbg!((getport(&s1), getport(&s2)));

    try_join!(s1.connect((addr, p2)), s2.connect((addr, p1))).expect("connect failed");

    let mut bufa = [MaybeUninit::new(0); 6];
    let mut buf1 = MsgBuf::from(bufa.as_mut());
    let mut abuf1 = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0).into();
    let mut buf2 = MsgBuf::from(Vec::with_capacity(16));
    let mut abuf2 = abuf1;

    let msg = "\
This message is definitely too huge for bufa, and will generally require multiple resizes unless \
the memory allocator decides to be smarter than usual and give us a huge buffer on the first try";

    let (ssz1, ssz2) =
        try_join!(s1.send(msg.as_bytes()), s2.send(msg.as_bytes())).expect("send failed");
    assert_eq!(ssz1, msg.len());
    assert_eq!(ssz2, msg.len());

    let comck = |rslt, buf: &mut MsgBuf<'_>| {
        dbg!(&*buf);
        dbg!(rslt);
        assert!(matches!(rslt, RecvResult::Spilled));
        assert_eq!(buf.len_filled(), msg.len());
        assert_eq!(from_utf8(buf.filled_part()).expect("invalid UTF-8"), msg);
    };
    let (rslt1, rslt2) = try_join!(
        s1.recv_msg(&mut buf1, Some(&mut abuf1)),
        s2.recv_msg(&mut buf2, Some(&mut abuf2))
    )
    .expect("receive failed");
    comck(rslt1, &mut buf1);
    comck(rslt2, &mut buf2);

    dbg!((&abuf1, &abuf2));
    assert!(abuf1.ip().is_loopback());
    assert!(abuf2.ip().is_loopback());
    assert_eq!(abuf1.port(), p2);
    assert_eq!(abuf2.port(), p1);
}
