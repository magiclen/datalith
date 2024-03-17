mod datalith;
mod datalith_errors;
mod functions;

pub use datalith::*;
pub use datalith_errors::*;
use mime::{Mime, APPLICATION_OCTET_STREAM};

/// The default mime type.
pub const DEFAULT_MIME_TYPE: Mime = APPLICATION_OCTET_STREAM;

/// A string of an encrypted file ID which can be used as a URL component.
pub type IDToken = String;
