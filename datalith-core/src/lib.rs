mod datalith;
mod datalith_errors;
mod datalith_file;
mod functions;
mod guard;
#[cfg(feature = "magic")]
mod magic_cookie_pool;

pub use datalith::*;
pub use datalith_errors::*;
pub use datalith_file::*;
pub use mime;
use mime::{Mime, APPLICATION_OCTET_STREAM};
pub use uuid;

/// The default mime type.
pub const DEFAULT_MIME_TYPE: Mime = APPLICATION_OCTET_STREAM;

/// A string of an encrypted file ID which can be used as a URL component.
pub type IDToken = String;
