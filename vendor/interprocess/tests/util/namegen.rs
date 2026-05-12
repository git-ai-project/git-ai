use {
    super::Xorshift32,
    crate::local_socket::{GenericFilePath, GenericNamespaced, Name, ToFsName, ToNsName},
    std::io,
};

#[derive(Copy, Clone, Debug)]
pub struct NameGen<T, F: FnMut(u32) -> io::Result<T>> {
    rng: Xorshift32,
    name_fn: F,
}
impl<T, F: FnMut(u32) -> io::Result<T>> NameGen<T, F> {
    pub fn new(id: &str, name_fn: F) -> Self { Self { rng: Xorshift32::from_id(id), name_fn } }
}
impl<T, F: FnMut(u32) -> io::Result<T>> Iterator for NameGen<T, F> {
    type Item = io::Result<T>;
    fn next(&mut self) -> Option<Self::Item> { Some((self.name_fn)(self.rng.next())) }
}

pub fn namegen_local_socket(
    id: &str,
    path: bool,
) -> NameGen<Name<'static>, impl FnMut(u32) -> io::Result<Name<'static>>> {
    NameGen::new(id, move |rn| if path { next_fs(rn) } else { next_ns(rn) })
}

fn next_fs(rn: u32) -> io::Result<Name<'static>> {
    if cfg!(windows) {
        windows_path(rn)
    } else if cfg!(unix) {
        unix_path(rn)
    } else {
        unreachable!()
    }
    .to_fs_name::<GenericFilePath>()
}
fn next_ns(rn: u32) -> io::Result<Name<'static>> {
    format!("interprocess-test-{:08x}", rn).to_ns_name::<GenericNamespaced>()
}

pub fn namegen_named_pipe(id: &str) -> NameGen<String, impl FnMut(u32) -> io::Result<String>> {
    NameGen::new(id, |rn| Ok(windows_path(rn)))
}

fn windows_path(rn: u32) -> String { format!(r"\\.\pipe\interprocess-test-{rn:08x}") }
fn unix_path(rn: u32) -> String {
    let tmpdir = std::env::var("TMPDIR").ok();
    format!("{}/interprocess-test-{rn:08x}.sock", tmpdir.as_deref().unwrap_or("/tmp"))
}

macro_rules! make_id {
    () => {
        concat!(file!(), line!(), column!())
    };
}
