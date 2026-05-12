#![allow(deprecated)]

use {
    crate::{
        local_socket::{prelude::*, ListenerOptions, Stream},
        os::unix::local_socket::SpecialDirUdSocket,
        tests::util::*,
    },
    std::sync::Arc,
};

fn test_inner(iter: u32) -> TestResult {
    let mut namegen = NameGen::new(&format!("{}{}", make_id!(), iter), |rnum| {
        format!("interprocess test {:08x}/fake ns/test.sock", rnum)
            .to_ns_name::<SpecialDirUdSocket>()
            .map(Arc::new)
    });
    let (name, _listener) = listen_and_pick_name(&mut namegen, |nm| {
        ListenerOptions::new().name(nm.borrow()).create_sync()
    })?;
    let name = Arc::try_unwrap(name).unwrap();
    let _ = Stream::connect(name.borrow()).opname("client connect")?;

    Ok(())
}

#[test]
fn main() -> TestResult {
    test_wrapper(|| {
        // fucking macOS
        let iterations = if cfg!(target_os = "macos") { 444 } else { 32 };
        for i in 0..iterations {
            test_inner(i)?;
        }
        Ok(())
    })
}
