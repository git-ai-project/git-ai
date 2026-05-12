/// Only dispatches with `&self` and `&mut self`.
macro_rules! dispatch {
    (@$arm:ident $nm:ident $e:expr) => {{
        let mut _arm2 = $arm;
        let $nm = &mut _arm2;
        $e
    }};
    ($ty:ident: $nm:ident in $var:expr => $e:expr) => {{
        match $var {
            #[cfg(windows)]
            $ty::NamedPipe(arm) => dispatch!(@arm $nm $e),
            #[cfg(unix)]
            $ty::UdSocket(arm) => dispatch!(@arm $nm $e),
        }
    }};
}

macro_rules! mkenum {
    ($(#[$($attr:tt)+])* $pref:literal $nm:ident) => {
        $(#[$($attr)+])*
        pub enum $nm {
            /// Makes use of Windows named pipes.
            ///
            /// Click the struct name in the parentheses to learn more.
            #[cfg(windows)]
            #[cfg_attr(feature = "doc_cfg", doc(cfg(windows)))]
            NamedPipe(np_impl::$nm),
            /// Makes use of Unix domain sockets.
            ///
            /// Click the struct name in the parentheses to learn more.
            #[cfg(unix)]
            #[cfg_attr(feature = "doc_cfg", doc(cfg(unix)))]
            UdSocket(uds_impl::$nm),
        }
        impl $crate::Sealed for $nm {}
        #[cfg(windows)]
        #[cfg_attr(feature = "doc_cfg", doc(cfg(windows)))]
        impl From<np_impl::$nm> for $nm {
            fn from(x: np_impl::$nm) -> Self {
                Self::NamedPipe(x)
            }
        }
        #[cfg(unix)]
        #[cfg_attr(feature = "doc_cfg", doc(cfg(unix)))]
        impl From<uds_impl::$nm> for $nm {
            fn from(x: uds_impl::$nm) -> Self {
                Self::UdSocket(x)
            }
        }
        impl std::fmt::Debug for $nm {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                let mut dt = f.debug_tuple(concat!($pref, stringify!($nm)));
                dispatch!($nm: i in self => dt.field(&i));
                dt.finish()
            }
        }
    };
    ($(#[$($attr:tt)+])* $nm:ident) => {
        mkenum!($(#[$($attr)+])* "" $nm);
    };
}
