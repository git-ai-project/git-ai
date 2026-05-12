use core::mem::size_of;
use std::{
    io::{self, ErrorKind::InvalidInput},
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6},
};
use windows_sys::Win32::Networking::WinSock::{
    socklen_t, AF_INET, AF_INET6, SOCKADDR_IN, SOCKADDR_IN6, SOCKADDR_STORAGE,
};

fn from_be_16(i: u16) -> u16 {
    u16::from_be_bytes(i.to_ne_bytes())
}
fn from_be_32(i: u32) -> u32 {
    u32::from_be_bytes(i.to_ne_bytes())
}

pub(super) fn extract_ip_address(
    addr: &SOCKADDR_STORAGE,
    len: socklen_t,
) -> io::Result<SocketAddr> {
    let paddr = addr as *const SOCKADDR_STORAGE;
    const SIZE4: socklen_t = size_of::<SOCKADDR_IN>() as _;
    const SIZE6: socklen_t = size_of::<SOCKADDR_IN6>() as _;
    match (addr.ss_family as _, len) {
        (AF_INET, SIZE4) => unsafe {
            let addr = &*paddr.cast::<SOCKADDR_IN>();
            Ok(SocketAddrV4::new(
                Ipv4Addr::from(from_be_32(addr.sin_addr.S_un.S_addr)),
                from_be_16(addr.sin_port),
            )
            .into())
        },
        (AF_INET6, SIZE6) => unsafe {
            let addr = &*paddr.cast::<SOCKADDR_IN6>();
            Ok(SocketAddrV6::new(
                Ipv6Addr::from(addr.sin6_addr.u.Byte),
                from_be_16(addr.sin6_port),
                from_be_32(addr.sin6_flowinfo),
                from_be_32(addr.Anonymous.sin6_scope_id),
            )
            .into())
        },
        _ => Err(io::Error::from(InvalidInput)),
    }
}
