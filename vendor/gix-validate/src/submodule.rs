use bstr::{BStr, ByteSlice};

///
pub mod name {
    /// The error used in [name()](super::name()).
    #[derive(Debug)]
    #[allow(missing_docs)]
    #[non_exhaustive]
    pub enum Error {
        Empty,
        ParentComponent,
    }

    impl std::fmt::Display for Error {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Error::Empty => write!(f, "Submodule names cannot be empty"),
                Error::ParentComponent => write!(f, "Submodules names must not contains '..'"),
            }
        }
    }

    impl std::error::Error for Error {}
}

/// Return the original `name` if it is valid, or the respective error indicating what was wrong with it.
pub fn name(name: &BStr) -> Result<&BStr, name::Error> {
    if name.is_empty() {
        return Err(name::Error::Empty);
    }
    match name.find(b"..") {
        Some(pos) => {
            let &b = name.get(pos + 2).ok_or(name::Error::ParentComponent)?;
            if b == b'/' || b == b'\\' {
                Err(name::Error::ParentComponent)
            } else {
                Ok(name)
            }
        }
        None => Ok(name),
    }
}
