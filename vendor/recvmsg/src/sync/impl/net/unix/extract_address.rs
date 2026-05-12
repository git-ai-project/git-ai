use core::mem::{size_of, transmute};
use libc::{
    sockaddr_in, sockaddr_in6, sockaddr_storage, sockaddr_un, socklen_t, AF_INET, AF_INET6, AF_UNIX,
};
use std::{
    io::{self, ErrorKind::InvalidInput},
    net::{Ipv4Addr, Ipv6Addr, SocketAddr as InetAddr, SocketAddrV4, SocketAddrV6},
    os::unix::net::SocketAddr as UnixAddr,
};

fn from_be_16(i: u16) -> u16 {
    u16::from_be_bytes(i.to_ne_bytes())
}
fn from_be_32(i: u32) -> u32 {
    u32::from_be_bytes(i.to_ne_bytes())
}

pub(crate) fn extract_ip_address(addr: &sockaddr_storage, len: socklen_t) -> io::Result<InetAddr> {
    let paddr = addr as *const sockaddr_storage;
    const SIZE4: socklen_t = size_of::<sockaddr_in>() as _;
    const SIZE6: socklen_t = size_of::<sockaddr_in6>() as _;
    match (addr.ss_family as _, len) {
        (AF_INET, SIZE4) => {
            let addr = unsafe { &*paddr.cast::<sockaddr_in>() };
            Ok(SocketAddrV4::new(
                Ipv4Addr::from(from_be_32(addr.sin_addr.s_addr)),
                from_be_16(addr.sin_port),
            )
            .into())
        }
        (AF_INET6, SIZE6) => {
            let addr = unsafe { &*paddr.cast::<sockaddr_in6>() };
            Ok(SocketAddrV6::new(
                Ipv6Addr::from(addr.sin6_addr.s6_addr),
                from_be_16(addr.sin6_port),
                from_be_32(addr.sin6_flowinfo),
                from_be_32(addr.sin6_scope_id),
            )
            .into())
        }
        _ => Err(io::Error::from(InvalidInput)),
    }
}
pub(crate) fn extract_unix_address(
    addr: &sockaddr_storage,
    len: socklen_t,
) -> io::Result<UnixAddr> {
    let paddr = addr as *const sockaddr_storage;
    if AF_UNIX == addr.ss_family as _ {
        let addr = unsafe { &*paddr.cast::<sockaddr_un>() };
        Ok(unix_addr_from_raw_parts(*addr, len))
    } else {
        Err(io::Error::from(InvalidInput))
    }
}

fn unix_addr_from_raw_parts(addr: sockaddr_un, len: socklen_t) -> UnixAddr {
    #[allow(dead_code)]
    struct FakeUnixAddr {
        addr: sockaddr_un,
        len: socklen_t,
    }
    unsafe {
        // FIXME this is UB.
        transmute(FakeUnixAddr { addr, len })
    }
}
