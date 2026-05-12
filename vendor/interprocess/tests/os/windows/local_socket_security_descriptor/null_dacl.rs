use {
    crate::{
        local_socket::{prelude::*, ListenerOptions, Stream},
        os::windows::{
            local_socket::ListenerOptionsExt,
            security_descriptor::{AsSecurityDescriptorMutExt, SecurityDescriptor},
        },
        tests::util::*,
        TryClone,
    },
    std::ptr,
};

pub(super) fn test_main() -> TestResult {
    let mut sd = SecurityDescriptor::new().opname("security descriptor creation")?;
    unsafe {
        sd.set_dacl(ptr::null_mut(), false).opname("DACL setter")?;
    }
    let (name, _listener) =
        listen_and_pick_name(&mut namegen_local_socket(make_id!(), false), |nm| {
            ListenerOptions::new()
                .name(nm.borrow())
                .security_descriptor(sd.try_clone()?)
                .create_sync()
        })?;
    let _ = Stream::connect(name).opname("client connect")?;
    Ok(())
}
