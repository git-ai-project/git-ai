use {
    crate::{local_socket::ListenerOptions, tests::util::*},
    color_eyre::eyre::WrapErr as _,
};

#[test]
fn main() -> TestResult {
    let name = namegen_local_socket(make_id!(), true)
        .next()
        .unwrap()
        .context("failed to select name")?;
    let _l1 = ListenerOptions::new()
        .name(name.borrow())
        .create_sync()
        .context("failed to create first listener")?;
    ListenerOptions::new()
        .name(name.borrow())
        .try_overwrite(true)
        .create_sync()
        .context("failed to create second listener")?;
    Ok(())
}
