use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "lowercase")]
pub enum TwilioMessage {
    Connected(Connected),
    Start(StartMessage),
    Media(MediaMessage),
    Stop(StopMessage),
    Mark(MarkMessage),
    Clear(ClearMessage),
    Dtmf(DtmfMessage),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Connected {
    pub protocol: String,
    pub version: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sequence_number: Option<String>,
    pub stream_sid: String,
    pub start: StartData,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartData {
    pub account_sid: String,
    pub stream_sid: String,
    pub call_sid: String,
    pub tracks: Vec<String>,
    pub media_format: MediaFormat,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaFormat {
    pub encoding: String,
    pub sample_rate: u32,
    pub channels: u8,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sequence_number: Option<String>,
    pub stream_sid: String,
    pub media: MediaData,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MediaData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    pub payload: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sequence_number: Option<String>,
    pub stream_sid: String,
    pub stop: StopData,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopData {
    pub account_sid: String,
    pub call_sid: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkMessage {
    pub stream_sid: String,
    pub mark: MarkData,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MarkData {
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearMessage {
    pub stream_sid: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DtmfMessage {
    pub stream_sid: String,
    pub dtmf: DtmfData,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DtmfData {
    pub digit: String,
}
