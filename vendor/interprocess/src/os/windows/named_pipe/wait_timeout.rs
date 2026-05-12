use std::time::Duration;

/// A [named pipe wait timeout][npw].
///
/// [npw]: https://learn.microsoft.com/en-nz/windows/win32/api/namedpipeapi/nf-namedpipeapi-waitnamedpipew
#[repr(transparent)] // #[repr(u32)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct WaitTimeout(u32);
impl WaitTimeout {
    /// Default wait timeout.
    ///
    /// If specified on the client, uses the default wait timeout specified by the server. If
    /// the server also specifies this value, Windows defaults to **50 milliseconds**.
    pub const DEFAULT: Self = Self(0x00000000);
    /// Wait for the shortest possible amount of time.
    pub const MIN: Self = Self(1);
    /// Wait for the largest possible finite amount of time.
    pub const MAX: Self = Self(Self::FOREVER.0 - 1);
    /// Wait indefinitely.
    pub const FOREVER: Self = Self(0xffffffff);

    /// Constructs from a raw value (given in milliseconds).
    ///
    /// See [`DEFAULT`](Self::DEFAULT) and [`FOREVER`](Self::FOREVER).
    #[inline(always)]
    pub const fn from_raw(raw: u32) -> Self { Self(raw) }
    /// Constructs from a number of milliseconds, clamping to [`MIN`](Self::MIN) and
    /// [`MAX`](Self::MAX) if needed.
    #[inline(always)]
    pub const fn from_millis_clamped(millis: u32) -> Self {
        match Self(millis) {
            Self::DEFAULT => Self::MIN,
            Self::FOREVER => Self::MAX,
            fits => fits,
        }
    }
    /// Like [`from_millis_clamped`](Self::from_millis_clamped), but constructs from a
    /// [`Duration`].
    pub const fn from_duration_clamped(duration: Duration) -> Self {
        let millis = duration.as_millis();
        if millis == Self::DEFAULT.0 as u128 {
            Self::MIN
        } else if millis >= u32::MAX as u128 {
            Self::MAX
        } else {
            // FUTURE use TryFrom
            #[allow(clippy::cast_possible_truncation)]
            Self(millis as u32)
        }
    }
    /// Returns the contained raw value (given in milliseconds).
    ///
    /// See [`DEFAULT`](Self::DEFAULT) and [`FOREVER`](Self::FOREVER).
    #[inline(always)]
    pub const fn to_raw(self) -> u32 { self.0 }
}
impl From<WaitTimeout> for u32 {
    #[inline(always)]
    fn from(x: WaitTimeout) -> Self { x.to_raw() }
}
