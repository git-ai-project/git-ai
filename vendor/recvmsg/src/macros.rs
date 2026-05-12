macro_rules! forward_trait_methods {
    ($($fnty:ident $mnm:ident $({$($fgen:tt)*})? ($($param:tt)*) $(-> $ret:ty)?;)+) => {
        $(forward_trait_method!($fnty $mnm $({$($fgen)*})? ($($param)*) $(-> $ret)?);)+
    };
}

macro_rules! forward_trait_method {
    (fn $mnm:ident $({$($fgen:tt)*})? (&self $(, $param:ident : $pty:ty),* $(,)?) $(-> $ret:ty)?) => {
        #[inline(always)]
        fn $mnm $(<$($fgen)*>)? (&self, $($param: $pty),*) $(-> $ret)? {
            (**self).$mnm($($param),*)
        }
    };
    (fn $mnm:ident $({$($fgen:tt)*})? (&mut self $(, $param:ident : $pty:ty)* $(,)?) $(-> $ret:ty)?) => {
        #[inline(always)]
        fn $mnm $(<$($fgen)*>)? (&mut self, $($param: $pty),*) $(-> $ret)? {
            (**self).$mnm($($param),*)
        }
    };
    (fn $mnm:ident $({$($fgen:tt)*})? (self $(, $param:ident : $pty:ty)* $(,)?) $(-> $ret:ty)?) => {
        #[inline(always)]
        fn $mnm $(<$($fgen)*>)? (self, $($param: $pty),*) $(-> $ret)? {
            (*self).$mnm($($param),*)
        }
    };
    (pin_fn $mnm:ident $({$($fgen:tt)*})? (self: Pin<&mut Self> $(, $param:ident : $pty:ty)* $(,)?) $(-> $ret:ty)?) => {
        #[inline(always)]
        fn $mnm $(<$($fgen)*>)? (self: Pin<&mut Self>, $($param: $pty),*) $(-> $ret)? {
            self.get_mut().as_mut().$mnm($($param),*)
        }
    };
    (deref_fn $mnm:ident $({$($fgen:tt)*})? (self: Pin<&mut Self> $(, $param:ident : $pty:ty)* $(,)?) $(-> $ret:ty)?) => {
        #[inline(always)]
        fn $mnm $(<$($fgen)*>)? (self: Pin<&mut Self>, $($param: $pty),*) $(-> $ret)? {
            Pin::new(&mut **self.get_mut()).$mnm($($param),*)
        }
    };
    (fn $mnm:ident $({$($fgen:tt)*})? ($($param:ident : $pty:ty),* $(,)?) $(-> $ret:ty)?) => {
        #[inline(always)]
        fn $mnm $(<$($fgen)*>)? ($($param: $pty),*) $(-> $ret)? {
            T::$mnm($($param),*)
        }
    };
}
