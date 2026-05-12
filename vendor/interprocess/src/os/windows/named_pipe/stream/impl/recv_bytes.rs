use {
    super::*,
    crate::{
        os::windows::{c_wrappers, downgrade_eof},
        AsBuf,
    },
};

impl RawPipeStream {
    #[track_caller]
    fn read(&self, buf: &mut (impl AsBuf + ?Sized)) -> io::Result<usize> {
        c_wrappers::read_exsync(self.as_handle(), buf)
    }
}

impl<Sm: PipeModeTag> PipeStream<pipe_mode::Bytes, Sm> {
    /// Same as `.read()` from the [`Read`] trait, but accepts an uninitialized buffer.
    #[inline]
    pub fn read_to_uninit(&self, buf: &mut [MaybeUninit<u8>]) -> io::Result<usize> {
        downgrade_eof(self.raw.get().read(buf))
    }
}

impl<Sm: PipeModeTag> Read for &PipeStream<pipe_mode::Bytes, Sm> {
    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        downgrade_eof(self.raw.get().read(buf))
    }
}
impl<Sm: PipeModeTag> Read for PipeStream<pipe_mode::Bytes, Sm> {
    #[inline(always)]
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> { (&*self).read(buf) }
}
