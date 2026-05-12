use super::*;
use crate::panic_try_recv_retcon;

/// Implements [`TruncatingRecvMsg::recv_trunc()`] via
/// [`TruncatingRecvMsgWithFullSize::recv_trunc_with_full_size()`].
pub fn recv_trunc_via_recv_trunc_with_full_size<TRMWFS: TruncatingRecvMsgWithFullSize + ?Sized>(
    slf: &mut TRMWFS,
    peek: bool,
    buf: &mut MsgBuf<'_>,
    abuf: Option<&mut TRMWFS::AddrBuf>,
) -> Result<Option<bool>, TRMWFS::Error> {
    let cap = buf.len();
    let rslt = slf.recv_trunc_with_full_size(peek, buf, abuf)?;
    debug_assert_eq!(buf.len(), cap, "`recv_trunc_with_size()` changed buffer capacity");
    Ok(match rslt {
        TryRecvResult::Fit => Some(true),
        TryRecvResult::Spilled(..) => Some(false),
        TryRecvResult::EndOfStream => None,
    })
}

/// Implements [`RecvMsg::recv_msg()`] via [`TruncatingRecvMsg::recv_trunc()`].
pub fn recv_via_recv_trunc<TRM: TruncatingRecvMsg + ?Sized>(
    slf: &mut TRM,
    buf: &mut MsgBuf<'_>,
    mut abuf: Option<&mut TRM::AddrBuf>,
) -> Result<RecvResult, TRM::Error> {
    let mut fit_first = true;
    loop {
        let fit = match slf.recv_trunc(true, buf, abuf.as_deref_mut()) {
            Ok(Some(fit)) => fit,
            Ok(None) => return Ok(RecvResult::EndOfStream),
            Err(e) => {
                buf.set_fill(0);
                buf.has_msg = false;
                return Err(e);
            }
        };
        if fit {
            break;
        } else {
            fit_first = false;
            buf.set_fill(0);
            if let Err(qe) = buf.clear_and_grow() {
                return Ok(RecvResult::QuotaExceeded(qe));
            }
        }
    }
    slf.discard_msg()?;
    Ok(if fit_first { RecvResult::Fit } else { RecvResult::Spilled })
}

/// Implements [`RecvMsg::recv_msg()`] via [`TruncatingRecvMsgWithFullSizeExt::try_recv_msg()`].
pub fn recv_via_try_recv<TRMWFS: TruncatingRecvMsgWithFullSize + ?Sized>(
    slf: &mut TRMWFS,
    buf: &mut MsgBuf<'_>,
    mut abuf: Option<&mut TRMWFS::AddrBuf>,
) -> Result<RecvResult, TRMWFS::Error> {
    let ok = match slf.try_recv_msg(buf, abuf.as_deref_mut())? {
        TryRecvResult::Spilled(sz) => {
            if let Err(qe) = buf.clear_and_grow_to(sz) {
                return Ok(RecvResult::QuotaExceeded(qe));
            }
            match slf.try_recv_msg(buf, abuf)? {
                TryRecvResult::Fit => RecvResult::Spilled,
                TryRecvResult::Spilled(..) => panic_try_recv_retcon(),
                TryRecvResult::EndOfStream => return Ok(RecvResult::EndOfStream),
            }
        }
        fit_or_end => fit_or_end.into(),
    };
    Ok(ok)
}
