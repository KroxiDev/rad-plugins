//! Pure one-frame framing and request envelope validation; no transport or authority.
mod json;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    InvalidLength,
}
#[derive(Clone, Copy)]
pub struct Frame<'a> {
    payload: &'a [u8],
    consumed: usize,
}
impl<'a> Frame<'a> {
    pub fn payload(self) -> &'a [u8] {
        self.payload
    }
    pub fn consumed(self) -> usize {
        self.consumed
    }
}
/// Read at most one frame without allocation. None consumes nothing.
/// InvalidLength requires the transport to close; EOF/partial deadlines are external.
pub fn read_frame(input: &[u8]) -> Result<Option<Frame<'_>>, FrameError> {
    let Some(header) = input.get(..4) else {
        return Ok(None);
    };
    let length = u32::from_be_bytes(header.try_into().unwrap()) as usize;
    if !(1..=65536).contains(&length) {
        return Err(FrameError::InvalidLength);
    }
    let Some(payload) = input.get(4..4 + length) else {
        return Ok(None);
    };
    Ok(Some(Frame {
        payload,
        consumed: 4 + length,
    }))
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    InvalidRequest,
    UnsupportedVersion,
    Closed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Safe response metadata; id=None means close without a correlated response.
/// Closed is local state, not a protocol error code to send.
pub struct RequestError {
    pub code: ErrorCode,
    pub id: Option<u64>,
    pub close: bool,
}
#[derive(Clone, PartialEq)]
pub struct Request {
    id: u64,
    operation: String,
    args: serde_json::Map<String, serde_json::Value>,
}
impl Request {
    pub fn id(&self) -> u64 {
        self.id
    }
    pub fn operation(&self) -> &str {
        &self.operation
    }
    /// Structurally bounded only. Operation DTO and authorization are NOT validated.
    pub fn args(&self) -> &serde_json::Map<String, serde_json::Value> {
        &self.args
    }
}
#[derive(Debug, Default)]
/// Per-connection request ordering, not runtime identity or endpoint authorization.
pub struct Session {
    last: Option<u64>,
    closed: bool,
}
impl Session {
    pub fn accept(&mut self, frame: Frame<'_>) -> Result<Request, RequestError> {
        if self.closed {
            return Err(RequestError {
                code: ErrorCode::Closed,
                id: None,
                close: true,
            });
        }
        let result = self.parse(frame);
        if result.as_ref().is_err_and(|error| error.close) {
            self.closed = true;
        }
        result
    }
    fn parse(&mut self, frame: Frame<'_>) -> Result<Request, RequestError> {
        let invalid = || RequestError {
            code: ErrorCode::InvalidRequest,
            id: None,
            close: true,
        };
        let value: serde_json::Value = json::parse(frame.payload()).map_err(|_| invalid())?;
        let object = value.as_object().ok_or_else(invalid)?;
        if object.len() != 4
            || !["v", "id", "op", "args"]
                .iter()
                .all(|k| object.contains_key(*k))
        {
            return Err(invalid());
        }
        let version = object["v"].as_u64().ok_or_else(invalid)?;
        let id = object
            .get("id")
            .and_then(|v| v.as_str())
            .and_then(canonical_id)
            .ok_or_else(invalid)?;
        if self.last.is_some_and(|last| id <= last) {
            return Err(RequestError {
                code: ErrorCode::InvalidRequest,
                id: Some(id),
                close: true,
            });
        }
        let operation = object
            .get("op")
            .and_then(|v| v.as_str())
            .ok_or_else(invalid)?
            .to_owned();
        if operation.is_empty() {
            return Err(invalid());
        }
        if version != 1 {
            return Err(RequestError {
                code: ErrorCode::UnsupportedVersion,
                id: Some(id),
                close: true,
            });
        }
        let args = match object.get("args").and_then(|v| v.as_object()) {
            Some(args) => args.clone(),
            None => {
                let close = self.last.is_none() || operation == "hello";
                self.last = Some(id);
                return Err(RequestError {
                    code: ErrorCode::InvalidRequest,
                    id: Some(id),
                    close,
                });
            }
        };
        if self.last.is_none() {
            if operation != "hello" || id != 1 || !args.is_empty() {
                return Err(RequestError {
                    code: ErrorCode::InvalidRequest,
                    id: Some(id),
                    close: true,
                });
            }
        } else if operation == "hello" {
            return Err(RequestError {
                code: ErrorCode::InvalidRequest,
                id: Some(id),
                close: true,
            });
        }
        self.last = Some(id);
        Ok(Request {
            id,
            operation,
            args,
        })
    }
}

fn canonical_id(text: &str) -> Option<u64> {
    if text.is_empty() || text.starts_with('0') || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// A sanitized structural JSON error; never includes input or parser diagnostics.
pub struct JsonError;

/// Parse one bounded JSON value with the same duplicate/depth/string/array rules
/// as request envelopes. Clients use this before normalizing JSON arguments or
/// decoding responses; operation validation and authorization remain external.
pub fn parse_json(bytes: &[u8]) -> Result<serde_json::Value, JsonError> {
    if !(1..=65536).contains(&bytes.len()) {
        return Err(JsonError);
    }
    json::parse(bytes).map_err(|_| JsonError)
}

impl std::fmt::Debug for Frame<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Frame([redacted])")
    }
}
impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}
