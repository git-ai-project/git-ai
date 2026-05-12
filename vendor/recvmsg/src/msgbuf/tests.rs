use super::MsgBuf;
use alloc::vec::Vec;
use core::mem::MaybeUninit;

#[test]
fn clear_and_grow() {
    let mut bufbak = [0; 1];
    let mut buf = MsgBuf::from(bufbak.as_mut());
    buf.clear_and_grow_to(2).unwrap();
    assert!(buf.capacity() >= 2);
    buf.clear_and_grow_to(309).unwrap();
    assert!(buf.capacity() >= 309);

    buf = MsgBuf::from(alloc::vec::Vec::with_capacity(305));
    buf.clear_and_grow_to(512).unwrap();
    assert!(buf.capacity() >= 512);
    buf.clear_and_grow_to(1025).unwrap();
    assert!(buf.capacity() >= 1025);

    buf.quota = Some(256);
    assert!(buf.clear_and_grow_to(4096).is_err());
    assert!(buf.capacity() >= 1025);
    buf.quota = Some(4096);
    buf.clear_and_grow_to(4096).unwrap();
    assert!(buf.capacity() >= 4096);

    buf = MsgBuf::from(bufbak.as_mut());
    buf.quota = Some(0);
    assert!(buf.clear_and_grow_to(1).is_ok());
    assert!(buf.clear_and_grow_to(2).is_err());
}

const REF: &[u8] = b"This is the string which is to be retained";

fn retain_check(buf: &mut MsgBuf<'_>) {
    buf.grow_to(REF.len() * 64).unwrap();
    assert_eq!(buf.filled_part(), REF);
}

#[test]
fn grow_slice() {
    let mut bufbak = [0; REF.len()];
    bufbak[..REF.len()].copy_from_slice(REF);
    let mut buf = MsgBuf::from(&mut bufbak[..]);
    buf.set_fill(REF.len());
    retain_check(&mut buf);
}

#[test]
fn grow_vec() {
    let mut bufbak = Vec::new();
    bufbak.extend_from_slice(REF);
    let mut buf = MsgBuf::from(bufbak);
    buf.set_fill(REF.len());
    retain_check(&mut buf);
}

#[test]
fn extend() {
    let mut bufbak = [MaybeUninit::uninit(); 10];
    let mut buf = MsgBuf::from(bufbak.as_mut());
    buf.extend_from_slice(&[1; 10]).unwrap();
    assert_eq!(buf.len_filled(), 10);
}

#[test]
fn drop() {
    // One tebibyte.
    for _ in 0..32768 {
        let _ = MsgBuf::with_capacity::<Vec<u8>>(1024 * 1024 * 32);
    }
}
