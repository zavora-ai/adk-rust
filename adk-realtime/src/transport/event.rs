use crate::audio::AudioChunk;

/// Events received from the transport layer.
#[derive(Debug, Clone)]
pub enum TransportEvent {
    /// The transport connection has been established.
    Started { call_id: Option<String>, stream_id: Option<String>, participant_id: Option<String> },
    /// Audio data received from the transport.
    Audio {
        chunk: AudioChunk,
        timestamp_ms: Option<u64>,
        sequence: Option<u64>,
        source: Option<String>,
    },
    /// A DTMF (Dual-Tone Multi-Frequency) digit received.
    Dtmf { digit: String, source: Option<String> },
    /// A specific point in the media stream was reached.
    Mark { name: String },
    /// The stream was interrupted (e.g. by barge-in).
    Interrupted,
    /// The transport connection stopped.
    Stopped { reason: Option<String> },
    /// An error occurred in the transport layer.
    Error { message: String },
}

/// Control messages sent to the transport layer.
#[derive(Debug, Clone)]
pub enum TransportControl {
    /// Clear the playback buffer.
    ClearQueue,
    /// Add a mark to the output stream.
    Mark { name: String },
    /// Mute the output.
    Mute(bool),
}
