use crate::audio::AudioFormat;

/// Configuration for a media transport.
#[derive(Debug, Clone)]
pub struct MediaTransportConfig {
    /// Expected audio format for input (transport -> model).
    pub input_format: AudioFormat,

    /// Expected audio format for output (model -> transport).
    pub output_format: AudioFormat,

    /// Output frame size in milliseconds. Default 20ms.
    pub output_frame_ms: u16,

    /// Output pre-buffer in milliseconds. e.g. 40-100ms.
    pub output_prebuffer_ms: u16,

    /// Whether to automatically silence output on barge-in.
    pub auto_silence: bool,

    /// Whether barge-in is enabled.
    pub barge_in: bool,

    /// Whether to drain the audio buffer when closing.
    pub drain_on_close: bool,
}

impl Default for MediaTransportConfig {
    fn default() -> Self {
        Self {
            input_format: AudioFormat::pcm16_24khz(),
            output_format: AudioFormat::pcm16_24khz(),
            output_frame_ms: 20,
            output_prebuffer_ms: 60,
            auto_silence: true,
            barge_in: true,
            drain_on_close: true,
        }
    }
}
