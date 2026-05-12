//! Local sockets, a socket-like IPC primitive in which clients access a server through a
//! filesystem path or an identifier inside a special namespace, with each client having a
//! private connection to the server.
//!
//! ## Implementations and dispatch
//! Local sockets are not a real IPC primitive implemented by the OS, but rather a construct of
//! Interprocess that is implemented in terms of an underlying IPC primitive. Different IPC
//! primitives are available on different platforms and have different capabilities and
//! limitations. As such, the types representing local sockets that you can find in this
//! module – [`Listener`], [`Stream`], [`RecvHalf`], [`SendHalf`] – are really enums in the style
//! of `enum_dispatch` that contain variants for all the different implementations of local
//! sockets that are available, and the types that they dispatch between are talked to via the
//! corresponding [`Listener`](traits::Listener), [`Stream`](traits::Stream)
//! [`RecvHalf`](traits::RecvHalf), [`SendHalf`](traits::SendHalf) traits that you can find in the
//! [`traits`] module. (Note that this dispatch is currently zero-cost on all platforms, as there
//! is only one underlying local socket implementation per platform, with Windows only using named
//! pipe based local sockets and Unix only using Unix-domain socket based local sockets, but this
//! may change in the future with the introduction of support for
//! [the Windows implementation of Unix-domain sockets][udswnd]. Even then, the overhead of this
//! dispatch is insignificant compared to the overhead of making the system calls that perform
//! the actual communication.)
//!
//! [udswnd]: https://devblogs.microsoft.com/commandline/af_unix-comes-to-windows/
//!
//! The [`prelude`] module is there to make it easier to handle all of this complexity without
//! suffering from naming collisions. **`use interprocess::local_socket::prelude::*;` is the
//! recommended way of bringing local sockets into scope.**
//!
//! ## Stability
//! Since interprocess communication cannot happen without agreement on a protocol between two or
//! more processes, the mapping of local sockets to underlying primitives is stable and
//! predictable. **The IPC primitive selected depends only on the current platform and the
//! [name type](NameType) used.** The mapping is trivial unless noted otherwise (in particular,
//! Interprocess never inserts its own message framing or any other type of metadata into the
//! stream – the bytes you write are the exact bytes that come out the other end), which means
//! that the portable API of local sockets is suitable for communicating with programs that do
//! not use Interprocess themselves, including programs not written in Rust. All you need to do
//! is use the correct name type for every platform.
//!
//! ## Raw handle and file descriptor access
//! The enum dispatchers purposely omit implementations of `{As,Into,From}Raw{Handle,Fd}`,
//! `As{Handle,Fd}`, `From<Owned{HandleFd}>` and `Into<Owned{Handle,Fd}>`. To access those trait
//! implementations on the underlying implementation types, you need to match on the enum. For
//! instance:
//! ```no_run
//! # #[cfg(unix)]
//! use {interprocess::local_socket::prelude::*, std::os::unix::prelude::*};
//! # #[cfg(unix)] fn hi(fd: OwnedFd, fd2: OwnedFd) {
//!
//! // Creating a stream from a file descriptor
//! let stream = LocalSocketStream::UdSocket(fd.into());
//! # let _ = stream;
//!
//! // Consuming a stream to get its file descriptor
//! let fd = match stream {
//!     LocalSocketStream::UdSocket(s) => OwnedFd::from(s),
//! };
//! # let _ = fd;
//!
//! # let stream = LocalSocketStream::UdSocket(fd2.into());
//! // Accessing a stream's file descriptor without taking ownership
//! let stream_impl = match &stream {
//!     LocalSocketStream::UdSocket(s) => s,
//! };
//! let fd = stream_impl.as_fd();
//! # let _ = fd;
//!
//! // Listener, RecvHalf, and SendHalf work analogously.
//! // Works just the same on Windows under the replacement of Fd with Handle.
//! # }
//! ```

#[macro_use]
mod enumdef;

mod name;
mod peer_creds;
mod stream {
    pub(super) mod r#enum;
    pub(super) mod options;
    pub(super) mod r#trait;
}
mod listener {
    pub(super) mod r#enum;
    pub(super) mod options;
    pub(super) mod r#trait;
}

/// Traits representing the interface of local sockets.
pub mod traits {
    pub use super::{
        listener::r#trait::{Listener, ListenerExt},
        stream::r#trait::*,
    };
    /// Traits for the Tokio variants of local socket objects.
    #[cfg(feature = "tokio")]
    #[cfg_attr(feature = "doc_cfg", doc(cfg(feature = "tokio")))]
    pub mod tokio {
        pub use super::super::tokio::{listener::r#trait::*, stream::r#trait::*};
    }
}

pub use {
    listener::{
        options::ListenerOptions,
        r#enum::*,
        r#trait::{Incoming, ListenerNonblockingMode},
    },
    name::*,
    peer_creds::*,
    stream::{options::ConnectOptions, r#enum::*},
};

/// Re-exports of [traits] done in a way that doesn't pollute the scope, as well as of the
/// enum-dispatch types with their names prefixed with `LocalSocket`.
pub mod prelude {
    pub use super::{
        name::{NameType as _, ToFsName as _, ToNsName as _},
        traits::{Listener as _, ListenerExt as _, Stream as _, StreamCommon as _},
        Listener as LocalSocketListener, Stream as LocalSocketStream,
    };
}

/// Asynchronous local sockets which work with the Tokio runtime and event loop.
///
/// The Tokio integration allows the local socket streams and listeners to be notified by the OS
/// kernel whenever they're ready to be received from of sent to, instead of requiring you to
/// spawn threads just to put them in a wait state of blocking on the I/O.
///
/// Everything said in the [documentation for sync local sockets](crate::local_socket) applies to
/// the Tokio versions of the corresponding items as well. Please read it before using Tokio-based
/// local sockets.
///
/// Types from this module will *not* work with other async runtimes, such as `async-std` or `smol`,
/// since the Tokio types' methods will panic whenever they're called outside of a Tokio runtime
/// context. Open an issue if you'd like to see other runtimes supported as well.
#[cfg(feature = "tokio")]
#[cfg_attr(feature = "doc_cfg", doc(cfg(feature = "tokio")))]
pub mod tokio {
    pub(super) mod listener {
        pub(in super::super) mod r#enum;
        pub(in super::super) mod r#trait;
    }
    pub(super) mod stream {
        pub(in super::super) mod r#enum;
        pub(in super::super) mod r#trait;
    }
    pub use {listener::r#enum::*, stream::r#enum::*};

    /// Like the [sync local socket prelude](super::prelude), but for Tokio local sockets.
    pub mod prelude {
        pub use super::{
            super::{
                name::{NameType as _, ToFsName as _, ToNsName as _},
                traits::{
                    tokio::{Listener as _, Stream as _},
                    StreamCommon as _,
                },
            },
            Listener as LocalSocketListener, Stream as LocalSocketStream,
        };
    }
}
