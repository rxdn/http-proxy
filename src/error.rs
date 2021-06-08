use http::{Error as HttpError, Uri};
use hyper::Error as HyperError;
use snafu::Snafu;
use twilight_http::{error::Error as TwilightError, routing::PathParseError};
use std::num::ParseIntError;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub (crate)))]
pub enum RequestError {
    Base64Error { source: base64::DecodeError },
    ChunkingRequest { source: HyperError },
    EncodingError { source: std::str::Utf8Error },
    InvalidPath { source: PathParseError },
    MakingResponseBody { source: HttpError },
    MethodNotAllowed { method: String },
    MissingAuthorization,
    NoPath { uri: Uri },
    ParseError { source: ParseIntError },
    RequestIssue { source: TwilightError },
}
