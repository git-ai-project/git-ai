use core::{
    fmt::{self, Display, Formatter},
    num::NonZeroUsize,
};

/// Error indicating that a buffer's memory allocation quota was exceeded during an operation that
/// had to perform a memory allocation.
#[derive(Copy, Clone, Debug)]
pub struct QuotaExceeded {
    /// The quota the buffer had at the time of the error.
    pub quota: usize,
    /// The size which the buffer was to attain.
    pub attempted_alloc: NonZeroUsize,
}
impl Display for QuotaExceeded {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let Self { quota, attempted_alloc } = self;
        write!(
            f,
            "quota of {quota} bytes exceeded by an attempted buffer reallocation to {attempted_alloc} bytes"
        )
    }
}
#[cfg(feature = "std")]
impl std::error::Error for QuotaExceeded {}
