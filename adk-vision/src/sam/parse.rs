//! The `sam-3.1` answer: server-sent events carrying special-token text, one line per frame.

use std::collections::BTreeMap;

/// How a mask payload is encoded, from the first character after `H,W,`.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SamMaskEncoding {
    /// `~`: the API default, coverage bytes.
    Lossless,
    /// `!`: one bit per pixel.
    OneBit,
}

impl SamMaskEncoding {
    /// The character the payload starts with on the wire, before [`SamMask::payload`].
    pub fn marker(self) -> char {
        match self {
            Self::Lossless => '~',
            Self::OneBit => '!',
        }
    }
}

/// One mask token: a `height` × `width` raster stretched over the record's box, with its
/// payload kept exactly as the API sent it. A small box's raster is the box at source
/// resolution; a large box's is downscaled (a 1699×398 box came back as 1024×240).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SamMask {
    /// Raster rows.
    pub height: u32,
    /// Raster columns.
    pub width: u32,
    /// The payload's encoding.
    pub encoding: SamMaskEncoding,
    /// The base85 payload after the encoding marker, opaque.
    pub payload: String,
}

/// One object on one frame: its identifier, its box and its mask.
#[derive(Clone, Debug, PartialEq)]
pub struct SamRecord {
    /// The frame index; always 0 for an image.
    pub frame: u64,
    /// The object identifier, stable across the frames of one response.
    pub object: String,
    /// `[left, top, right, bottom)` in source pixels: the wire's inclusive `x2` and `y2`
    /// plus one.
    pub bounds: [u32; 4],
    /// The source width and height the box is in.
    pub source: (u32, u32),
    /// The confidence, when the request asked for it.
    pub confidence: Option<f32>,
    /// The mask, stretched over `bounds`.
    pub mask: SamMask,
}

/// Parses the `output_text` lane into records as lines arrive, so a long video yields
/// records frame by frame.
///
/// Each line is `<Nf>` followed by records `id<|box;x1=..;y1=..;x2=..;y2=..;w=..;h=..|>
/// <|mask;x=0;y=0;data=H,W,<enc><payload>|>`, optionally comma-separated, per Meta's
/// published SAM 3 text format. A malformed line is an error: nothing is guessed.
///
/// # Example
///
/// ```
/// use adk_vision::sam::SamStreamParser;
///
/// let mut parser = SamStreamParser::new();
/// let line = "<0f>0<|box;x1=10;y1=20;x2=14;y2=24;w=100;h=80|><|mask;x=0;y=0;data=5,5,!!!!!(QO(0lu8?|>";
/// assert!(parser.push(&line[..30]).unwrap().is_empty());
/// let records = parser.push(&format!("{}\n", &line[30..])).unwrap();
/// assert_eq!(records[0].bounds, [10, 20, 15, 25]);
/// assert_eq!((records[0].mask.height, records[0].mask.width), (5, 5));
/// ```
#[derive(Debug, Default)]
pub struct SamStreamParser {
    buffer: String,
    text: String,
}

impl SamStreamParser {
    /// An empty parser.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a text delta and returns the records of every line it completed.
    ///
    /// # Errors
    ///
    /// A message naming the malformed line.
    pub fn push(&mut self, delta: &str) -> Result<Vec<SamRecord>, String> {
        self.text.push_str(delta);
        self.buffer.push_str(delta);
        let mut records = Vec::new();
        while let Some(end) = self.buffer.find('\n') {
            let line: String = self.buffer.drain(..=end).collect();
            records.extend(parse_line(line.trim_end_matches(['\n', '\r']))?);
        }
        Ok(records)
    }

    /// Ends the lane and returns the records of a last line with no newline.
    ///
    /// # Errors
    ///
    /// A message naming the malformed line.
    pub fn finish(&mut self) -> Result<Vec<SamRecord>, String> {
        let line = std::mem::take(&mut self.buffer);
        parse_line(line.trim())
    }

    /// Every character received so far.
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// Parses one line; an empty line has no records.
pub(crate) fn parse_line(line: &str) -> Result<Vec<SamRecord>, String> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(Vec::new());
    }
    let bad = |why: &str| format!("malformed sam-3.1 line ({why}): {}", preview(line));
    // The format keeps a line that does not start with `<` as text, not a record.
    let Some(rest) = line.strip_prefix('<') else {
        tracing::debug!(sam.line = %preview(line), "skipping a sam-3.1 text line");
        return Ok(Vec::new());
    };
    let close = rest.find('>').ok_or_else(|| bad("unclosed frame marker"))?;
    let marker = &rest[..close];
    let index = marker.split(';').next().unwrap_or_default();
    let frame: u64 = index
        .strip_suffix('f')
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| bad("frame marker is not <Nf>"))?;
    let mut rest = &rest[close + 1..];
    if rest.is_empty() {
        return Err(bad("a frame marker with no records"));
    }
    let mut records = Vec::new();
    while !rest.is_empty() {
        rest = rest.strip_prefix(',').unwrap_or(rest);
        let digits = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        if digits == 0 {
            return Err(bad("a record does not start with an object id"));
        }
        let object = rest[..digits].to_string();
        rest = &rest[digits..];
        let mut tokens: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        while let Some(body) = rest.strip_prefix("<|") {
            let end = body.find("|>").ok_or_else(|| bad("unclosed token"))?;
            let token = &body[..end];
            rest = &body[end + 2..];
            let (name, fields) = token.split_once(';').unwrap_or((token, ""));
            let mut map = BTreeMap::new();
            for field in fields.split(';') {
                let field = field.trim();
                if field.is_empty() {
                    continue;
                }
                let (k, v) = field.split_once('=').unwrap_or((field, ""));
                if map.insert(k.trim().to_string(), v.trim().to_string()).is_some() && k != "c" {
                    return Err(bad("a repeated field"));
                }
            }
            if matches!(name, "box" | "mask") && tokens.insert(name.to_string(), map).is_some() {
                return Err(bad("a repeated token"));
            }
        }
        if !rest.is_empty()
            && !rest.starts_with(',')
            && !rest.starts_with(|c: char| c.is_ascii_digit())
        {
            return Err(bad("text between tokens"));
        }
        let (Some(b), Some(m)) = (tokens.get("box"), tokens.get("mask")) else {
            return Err(bad("a record without one box and one mask"));
        };
        records.push(record(frame, object, b, m).map_err(|why| bad(&why))?);
    }
    Ok(records)
}

fn record(
    frame: u64,
    object: String,
    b: &BTreeMap<String, String>,
    m: &BTreeMap<String, String>,
) -> Result<SamRecord, String> {
    let int = |k: &str| -> Result<i64, String> {
        b.get(k)
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| format!("box `{k}` is missing or not an integer"))
    };
    let (x1, y1, x2, y2, w, h) =
        (int("x1")?, int("y1")?, int("x2")?, int("y2")?, int("w")?, int("h")?);
    if w <= 0 || h <= 0 || x1 < 0 || y1 < 0 || x2 < x1 || y2 < y1 || x2 >= w || y2 >= h {
        return Err(format!("box ({x1}, {y1})-({x2}, {y2}) does not lie inside {w}x{h}"));
    }
    let as_u32 = |v: i64| u32::try_from(v).map_err(|_| format!("{v} is out of range"));
    let bounds = [as_u32(x1)?, as_u32(y1)?, as_u32(x2 + 1)?, as_u32(y2 + 1)?];
    if m.get("x").map(String::as_str) != Some("0") || m.get("y").map(String::as_str) != Some("0") {
        return Err("mask `x` and `y` are not 0".into());
    }
    let data = m.get("data").ok_or("mask has no `data`")?;
    let mut parts = data.splitn(3, ',');
    let height: u32 =
        parts.next().and_then(|v| v.parse().ok()).ok_or("mask height is not a number")?;
    let width: u32 =
        parts.next().and_then(|v| v.parse().ok()).ok_or("mask width is not a number")?;
    let payload = parts.next().ok_or("mask has no payload")?;
    let mut chars = payload.chars();
    let encoding = match chars.next() {
        Some('~') => SamMaskEncoding::Lossless,
        Some('!') => SamMaskEncoding::OneBit,
        _ => return Err("mask payload has no encoding marker".into()),
    };
    if height == 0 || width == 0 {
        return Err("mask has no pixels".into());
    }
    let confidence =
        b.get("c").and_then(|c| c.parse::<f32>().ok()).filter(|c| (0.0..=1.0).contains(c));
    Ok(SamRecord {
        frame,
        object,
        bounds,
        source: (as_u32(w)?, as_u32(h)?),
        confidence,
        mask: SamMask { height, width, encoding, payload: chars.as_str().to_string() },
    })
}

fn preview(line: &str) -> String {
    let mut end = line.len().min(120);
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    if end < line.len() { format!("{}...", &line[..end]) } else { line.to_string() }
}

/// One server-sent event of a `/v1/responses` stream, reduced to what the client needs.
#[derive(Debug, PartialEq)]
pub(crate) enum SamEvent {
    /// Text for the output lane.
    Delta(String),
    /// The lane's final text.
    Done(String),
    /// The response completed; the images it reports processing.
    Completed { images: Option<u64> },
    /// The response failed or was cut short.
    Failed(String),
    /// Anything else.
    Other,
}

/// Splits a byte stream of server-sent events into their `data:` payloads.
#[derive(Debug, Default)]
pub(crate) struct SseBuffer {
    buffer: String,
}

impl SseBuffer {
    /// Adds bytes and returns every complete event's data.
    pub(crate) fn push(&mut self, chunk: &str) -> Vec<String> {
        self.buffer.push_str(&chunk.replace("\r\n", "\n"));
        let mut events = Vec::new();
        while let Some(end) = self.buffer.find("\n\n") {
            let block: String = self.buffer.drain(..end + 2).collect();
            let data: Vec<&str> = block
                .lines()
                .filter_map(|l| l.strip_prefix("data:"))
                .map(|d| d.strip_prefix(' ').unwrap_or(d))
                .collect();
            if !data.is_empty() {
                events.push(data.join("\n"));
            }
        }
        events
    }

    /// The data of a last event with no blank line after it.
    pub(crate) fn finish(&mut self) -> Vec<String> {
        let rest = std::mem::take(&mut self.buffer);
        let data: Vec<&str> = rest
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(|d| d.strip_prefix(' ').unwrap_or(d))
            .collect();
        if data.is_empty() { Vec::new() } else { vec![data.join("\n")] }
    }
}

/// Reads one event's data.
pub(crate) fn event(data: &str) -> Result<SamEvent, String> {
    if data.trim() == "[DONE]" {
        return Ok(SamEvent::Other);
    }
    let value: serde_json::Value =
        serde_json::from_str(data).map_err(|e| format!("an event is not JSON: {e}"))?;
    let kind = value.get("type").and_then(|t| t.as_str()).unwrap_or_default();
    Ok(match kind {
        "response.output_text.delta" => {
            SamEvent::Delta(value["delta"].as_str().unwrap_or_default().to_string())
        }
        "response.output_text.done" => {
            SamEvent::Done(value["text"].as_str().unwrap_or_default().to_string())
        }
        "response.completed" => {
            SamEvent::Completed { images: value["response"]["usage"]["images_processed"].as_u64() }
        }
        "response.failed" | "response.incomplete" | "error" => SamEvent::Failed(
            value
                .pointer("/response/error/message")
                .or_else(|| value.pointer("/error/message"))
                .or_else(|| value.pointer("/message"))
                .or_else(|| value.pointer("/response/incomplete_details/reason"))
                .and_then(|m| m.as_str())
                .unwrap_or(kind)
                .to_string(),
        ),
        "response.refusal.delta" | "response.refusal.done" => {
            SamEvent::Failed("the model refused the request".into())
        }
        _ => SamEvent::Other,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINE: &str =
        "<7f>0<|box;x1=10;y1=20;x2=14;y2=24;w=100;h=80|><|mask;x=0;y=0;data=5,5,!!!!!(QO(0lu8?|>";

    #[test]
    fn the_protocol_example_parses() {
        let records = parse_line(LINE).unwrap();
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert_eq!((r.frame, r.object.as_str()), (7, "0"));
        assert_eq!(r.bounds, [10, 20, 15, 25]);
        assert_eq!(r.source, (100, 80));
        assert_eq!(r.mask.encoding, SamMaskEncoding::OneBit);
        assert_eq!(r.mask.payload, "!!!!(QO(0lu8?");
    }

    #[test]
    fn confidences_and_several_records_parse() {
        let line = "<0f>0<|box;x1=1;y1=1;x2=2;y2=2;w=10;h=10;c=0.75|><|mask;x=0;y=0;c=0.75;data=2,2,~!!|>,\
                    2<|box;x1=3;y1=3;x2=3;y2=3;w=10;h=10|><|mask;x=0;y=0;data=1,1,~!|>";
        let records = parse_line(line).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].confidence, Some(0.75));
        assert_eq!(records[1].object, "2");
        assert_eq!(records[1].confidence, None);
        assert_eq!(records[1].mask.encoding, SamMaskEncoding::Lossless);
    }

    #[test]
    fn deltas_split_anywhere_give_the_same_records() {
        let text = format!("{LINE}\n{LINE}\n");
        let whole = SamStreamParser::new().push(&text).unwrap();
        for cut in [1, 17, 40, 88, text.len() - 1] {
            let mut parser = SamStreamParser::new();
            let mut records = parser.push(&text[..cut]).unwrap();
            records.extend(parser.push(&text[cut..]).unwrap());
            records.extend(parser.finish().unwrap());
            assert_eq!(records, whole, "cut at {cut}");
        }
        assert_eq!(whole.len(), 2);
    }

    #[test]
    fn malformed_lines_are_refused() {
        for line in [
            "<0f>",
            "<0f>0<|box;x1=1;y1=1;x2=2;y2=2;w=10;h=10|>",
            "<0f>0<|box;x1=1;y1=1;x2=20;y2=2;w=10;h=10|><|mask;x=0;y=0;data=2,20,~!|>",
            "<0f>0<|box;x1=1;y1=1;x2=2;y2=2;w=10;h=10|><|mask;x=1;y=0;data=2,2,~!|>",
            "<0f>0<|box;x1=1;y1=1;x2=2;y2=2;w=10;h=10|><|mask;x=0;y=0;data=0,3,~!|>",
            "<0f>0<|box;x1=1;y1=1;x2=2;y2=2;w=10;h=10|><|mask;x=0;y=0;data=2,2,#!|>",
            "<xf>0<|box;x1=1;y1=1;x2=2;y2=2;w=10;h=10|><|mask;x=0;y=0;data=2,2,~!|>",
            "<0f>0<|box;x1=1;y1=1;x2=2;y2=2;w=10;h=10|>junk<|mask;x=0;y=0;data=2,2,~!|>",
        ] {
            assert!(parse_line(line).is_err(), "{line}");
        }
        assert!(parse_line("").unwrap().is_empty());
        assert!(parse_line("a note the model wrote").unwrap().is_empty());
    }

    #[test]
    fn events_split_on_blank_lines() {
        let mut sse = SseBuffer::default();
        let first = sse.push(
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",",
        );
        assert!(first.is_empty());
        let events = sse.push("\"delta\":\"<0f>\"}\n\ndata: [DONE]\n\n");
        assert_eq!(events.len(), 2);
        assert_eq!(event(&events[0]).unwrap(), SamEvent::Delta("<0f>".into()));
        assert_eq!(event(&events[1]).unwrap(), SamEvent::Other);
        let done =
            event(r#"{"type":"response.completed","response":{"usage":{"images_processed":1}}}"#);
        assert_eq!(done.unwrap(), SamEvent::Completed { images: Some(1) });
        let failed = event(r#"{"type":"response.failed","response":{"error":{"message":"boom"}}}"#);
        assert_eq!(failed.unwrap(), SamEvent::Failed("boom".into()));
    }
}
