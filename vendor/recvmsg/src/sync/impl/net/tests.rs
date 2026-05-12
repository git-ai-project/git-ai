use crate::{MsgBuf, RecvMsg, RecvResult};
use std::{
    mem::MaybeUninit,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddrV4, UdpSocket},
    str::from_utf8,
};

#[test]
fn v4() {
    udp(false)
}
#[test]
fn v6() {
    udp(true)
}

fn udp(v6: bool) {
    let addr: IpAddr = if v6 { Ipv6Addr::LOCALHOST.into() } else { Ipv4Addr::LOCALHOST.into() };
    // The following two will choose different ports:
    let s1 = UdpSocket::bind((addr, 0)).expect("first bind failed");
    let s2 = UdpSocket::bind((addr, 0)).expect("second bind failed");

    let getport = |sock: &UdpSocket| sock.local_addr().expect("port query failed").port();
    let (p1, p2) = dbg!((getport(&s1), getport(&s2)));
    s1.connect((addr, p2)).expect("first connect failed");
    s2.connect((addr, p1)).expect("second connect failed");

    let mut bufa = [MaybeUninit::new(0); 6];
    let mut buf1 = MsgBuf::from(bufa.as_mut());
    let mut abuf1 = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0).into();
    let mut buf2 = MsgBuf::from(Vec::with_capacity(16));
    let mut abuf2 = abuf1;

    let msg = "\
This message is definitely too huge for bufa, and will generally require multiple resizes unless \
the memory allocator decides to be smarter than usual and give us a huge buffer on the first try";

    let ssz = s1.send(msg.as_bytes()).expect("first send failed");
    assert_eq!(ssz, msg.len());
    let ssz = s2.send(msg.as_bytes()).expect("second send failed");
    assert_eq!(ssz, msg.len());

    let comck = |rslt, buf: &mut MsgBuf<'_>| {
        dbg!(&*buf);
        dbg!(rslt);
        assert!(matches!(rslt, RecvResult::Spilled));
        assert_eq!(buf.len_filled(), msg.len());
        assert_eq!(from_utf8(buf.filled_part()).expect("invalid UTF-8"), msg);
    };
    let rslt =
        RecvMsg::recv_msg(&mut &s1, &mut buf1, Some(&mut abuf1)).expect("first receive failed");
    comck(rslt, &mut buf1);
    let rslt =
        RecvMsg::recv_msg(&mut &s2, &mut buf2, Some(&mut abuf2)).expect("second receive failed");
    comck(rslt, &mut buf2);

    dbg!((&abuf1, &abuf2));
    assert!(abuf1.ip().is_loopback());
    assert!(abuf2.ip().is_loopback());
    assert_eq!(abuf1.port(), p2);
    assert_eq!(abuf2.port(), p1);
}
