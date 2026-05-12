# `recvmsg`

[![Rust version: 1.60+](https://img.shields.io/badge/rust%20version-1.60+-orange)](https://blog.rust-lang.org/2021/10/21/Rust-1.60.0.html)

Traits for receiving datagrams reliably, without truncation.

## Problem
Unlike a byte stream interface, datagram sockets (most notably UDP) and other packet-based APIs
preserve boundaries between different write calls, which is what "message boundary" essentially
means. Extracting messages by partial reads is an error-prone task, which is why no such
interface is exposed by any OS – instead, all messages received from message IPC channels are
full messages rather than chunks of messages, which simplifies things to a great degree and is
arguably the only proper way of implementing datagram support.

There is one pecularity related to this design: you can't just use a buffer with arbitrary
length to successfully receive a message. With byte streams, that always works – there either is
some data which can be written into that buffer or end of file has been reached, aside from the
implied error case which is always a possibility for any kind of I/O. With datagrams,
however, **there might not always be enough space in a buffer to fetch a whole message**. If the
buffer is too small to fetch part of a message, it is truncated and the message ends up
essentially malformed.

## Solution
The [`RecvMsg`] trait (together with its async counterpart, [`AsyncRecvMsg`]) provides an
interface that completely prevents truncation.

With the help of [`MsgBuf`], a borrowed buffer can be provided, which can also be subsequently
transitioned into an owned buffer as needed. Alternatively, [`MsgBuf`] can start off with an
already-owned buffer. The inner `Vec` will then be resized as necessary, with an optional quota
preventing a connection from exhausting all memory.

## Implementation
There are three features a standard truncating message reception can provide to allow programs
to solve the truncation problem: peeking, truncation reporting and exact length querying. The
former two are represented by the [`TruncatingRecvMsg`] trait, while the last one can be seen as
an extension of those and is thus available as [`TruncatingRecvMsgWithFullSize`]. Both of those
have async counterparts.

[`RecvMsg`] or [`AsyncRecvMsg`] are then to be implemented in terms of either of those traits
using the appropriate helper function from the corresponding module.

## Feature flags
- *`std`* – `std::error::Error` on [`QuotaExceeded`]. Precludes `#![no_std]`.
- *`std_net`* – implementations of traits on types from `std::net` and `std::os::unix::net`
  (Unix domain sockets) on Unix.
