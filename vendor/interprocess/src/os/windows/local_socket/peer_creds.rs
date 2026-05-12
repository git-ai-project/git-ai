#[derive(Copy, Clone, Debug)]
pub struct PeerCreds {
    pub(crate) pid: u32,
}
impl PeerCreds {
    #[inline]
    pub fn pid(&self) -> Option<u32> { Some(self.pid) }
}

pub type Pid = u32;
