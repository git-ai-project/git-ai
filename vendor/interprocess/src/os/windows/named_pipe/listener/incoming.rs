use {
    super::{PipeListener, PipeModeTag, PipeStream},
    std::{io, iter::FusedIterator},
};

/// An iterator that infinitely [`.accept`](PipeListener::accept)s connections on a
/// [`PipeListener`].
///
/// This iterator is created by the [`.incoming()`](PipeListener::incoming) method on
/// [`PipeListener`]. See its documentation for more.
pub struct Incoming<'a, Rm: PipeModeTag, Sm: PipeModeTag>(pub(super) &'a PipeListener<Rm, Sm>);
impl<Rm: PipeModeTag, Sm: PipeModeTag> Iterator for Incoming<'_, Rm, Sm> {
    type Item = io::Result<PipeStream<Rm, Sm>>;
    #[inline]
    fn next(&mut self) -> Option<Self::Item> { Some(self.0.accept()) }
}
impl<'a, Rm: PipeModeTag, Sm: PipeModeTag> IntoIterator for &'a PipeListener<Rm, Sm> {
    type IntoIter = Incoming<'a, Rm, Sm>;
    type Item = <Incoming<'a, Rm, Sm> as Iterator>::Item;
    #[inline(always)]
    fn into_iter(self) -> Self::IntoIter { self.incoming() }
}
impl<Rm: PipeModeTag, Sm: PipeModeTag> FusedIterator for Incoming<'_, Rm, Sm> {}
