//! Windows-specific functionality for various interprocess communication primitives, as well as
//! Windows-specific ones.

pub mod local_socket;
pub mod named_pipe;
pub mod security_descriptor;
pub mod unnamed_pipe;
//pub mod mailslot;

mod impersonation_guard;
mod path_conversion;
mod share_handle;

pub use {impersonation_guard::*, path_conversion::*, share_handle::*};

pub(crate) mod misc;
mod adv_handle;
mod needs_flush;
mod linger_pool;
mod c_wrappers;
mod maybe_arc;
#[cfg(feature = "tokio")]
mod tokio_flusher;

pub(crate) use {maybe_arc::*, misc::*, needs_flush::*};
