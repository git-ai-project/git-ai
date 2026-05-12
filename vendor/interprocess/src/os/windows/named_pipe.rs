//! Support for named pipes on Windows.
//!
//! # Those are not Unix named pipes
//! The term "named pipe" refers to completely different things on Unix and on Windows. For this
//! reason, Unix named pipes are referred to as "FIFO files" to avoid confusion with the Windows
//! concept. In fact, the only common features for those two is that they both can be located using
//! filesystem paths and use a stream interface. One Unix concept that Windows named pipes do
//! resemble is Unix domain sockets, which is why named pipes act as
//! [the Windows implementation of local sockets](local_socket).
//!
//! # Semantic peculiarities
//! Methods and I/O trait implementations on types presented in this module do not exactly map 1:1
//! to Windows API system calls. [`PipeStream`] and [`PipeListener`], together with their async
//! counterparts, list important behavior implemented by Interprocess in their item-level
//! documentation.

// TODO improve docs
// TODO raw instance functionality
// TODO transactions

mod enums;
mod listener;
mod stream;
mod wait_timeout;

pub use {enums::*, listener::*, stream::*, wait_timeout::*};

/// Local sockets implemented using Windows named pipes.
pub mod local_socket {
    mod listener;
    mod stream;
    pub use {listener::*, stream::*};

    /// Async local sockets for Tokio implemented using named pipes.
    #[cfg(feature = "tokio")]
    pub mod tokio {
        mod listener;
        mod stream;
        pub use {listener::*, stream::*};
    }
}

mod c_wrappers;

/// Asynchronous named pipes which work with the Tokio runtime and event loop.
///
/// The Tokio integration allows the named pipe streams and listeners to be notified by the OS
/// kernel whenever they're ready to be received from or sent to, instead of spawning threads just
/// to put them in a wait state of blocking on the I/O.
///
/// Types from this module will *not* work with other async runtimes, such as `async-std` or `smol`,
/// since the Tokio types' methods will panic whenever they're called outside of a Tokio runtime
/// context. Open an issue if you'd like to see other runtimes supported as well.
#[cfg(feature = "tokio")]
#[cfg_attr(feature = "doc_cfg", doc(cfg(feature = "tokio")))]
pub mod tokio {
    mod listener;
    mod stream;
    pub use {listener::*, stream::*};
}
