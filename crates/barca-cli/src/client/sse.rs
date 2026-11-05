//! Server-sent events: the frames of `GET /runs/{run_id}/events`, parsed from the bytes as
//! they arrive.

#[cfg(test)]
#[path = "sse_tests.rs"]
mod tests;

/// SSE line framing keeps bytes until a complete line, including split UTF-8/CRLF.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct SseFrame {
    pub(super) id: Option<String>,
    pub(super) data: String,
}

#[derive(Default)]
pub(super) struct SseParser {
    line: Vec<u8>,
    data: Vec<String>,
    id: Option<String>,
    size: usize,
    cr: bool,
    seen_line: bool,
}
impl SseParser {
    pub(super) fn push(&mut self, bytes: &[u8]) -> Result<Vec<SseFrame>, String> {
        let mut out = Vec::new();
        for &byte in bytes {
            if self.cr {
                self.cr = false;
                if byte == b'\n' {
                    continue;
                }
            }
            if byte != b'\n' && byte != b'\r' {
                self.line.push(byte);
                if self.line.len() + self.size > 16 * 1024 * 1024 + 1024 {
                    return Err("event frame exceeds its size limit".into());
                }
                continue;
            }
            self.cr = byte == b'\r';
            let line = String::from_utf8(std::mem::take(&mut self.line))
                .map_err(|_| "event frame is not UTF-8")?;
            if line.is_empty() {
                if !self.data.is_empty() {
                    out.push(SseFrame {
                        id: self.id.clone(),
                        data: self.data.join("\n"),
                    });
                }
                self.data.clear();
                self.size = 0;
                continue;
            }
            let line = if !self.seen_line {
                self.seen_line = true;
                line.strip_prefix('\u{feff}').unwrap_or(&line)
            } else {
                &line
            };
            let (field, value) = line.split_once(':').unwrap_or((line, ""));
            let value = value.strip_prefix(' ').unwrap_or(value);
            match field {
                "data" => {
                    self.size += value.len() + 1;
                    self.data.push(value.into());
                }
                "id" if !value.contains('\0') => {
                    if value.len() > 128 {
                        return Err("event cursor exceeds its size limit".into());
                    }
                    self.id = (!value.is_empty()).then(|| value.into());
                }
                _ => {}
            }
        }
        Ok(out)
    }
}
