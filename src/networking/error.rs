use std::fmt;

#[derive(Debug)]
pub enum NetworkingError {
    /// A peer sent an invalid or unexpected response
    InvalidResponse,
    /// The transaction returned are invalid
    InvalidTransaction,
    /// The data stored in the block filters storage are corrupted
    DataCorruption,

    /// A peer is not connected
    NotConnected,
    /// A peer took too long to reply to one of our messages
    Timeout,

    /// Peer does not have bloom features enabled
    PeerBloomDisabled,

    /// No peers have been specified
    NoPeers,

    // Internal database error
    // Db(rocksdb::Error),
    /// Internal I/O error
    Io(std::io::Error),
    /// Internal system time error
    Time(std::time::SystemTimeError),
    // Wrapper for [`crate::error::Error`]
    // Global(Box<crate::error::Error>),
}
impl fmt::Display for NetworkingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl std::error::Error for NetworkingError {}

macro_rules! impl_error {
    ( $from:ty, $to:ident ) => {
        impl_error!($from, $to, Error);
    };
    ( $from:ty, $to:ident, $impl_for:ty ) => {
        impl std::convert::From<$from> for $impl_for {
            fn from(err: $from) -> Self {
                <$impl_for>::$to(err)
            }
        }
    };
}

impl_error!(std::io::Error, Io, NetworkingError);
impl_error!(std::time::SystemTimeError, Time, NetworkingError);
