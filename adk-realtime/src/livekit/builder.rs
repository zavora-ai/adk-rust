use crate::livekit::config::LiveKitConfig;
use crate::livekit::error::LiveKitError;
use livekit::prelude::{LocalAudioTrack, Room, RoomOptions, TrackSource};
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::prelude::{AudioSourceOptions, RtcAudioSource};
use std::marker::PhantomData;

/// A bundle containing the connected room, the event receiver, and optionally the configured audio track and source.
pub struct LiveKitRoomBundle {
    pub room: Room,
    pub events: tokio::sync::mpsc::UnboundedReceiver<livekit::prelude::RoomEvent>,
    pub audio_source: Option<NativeAudioSource>,
    pub audio_track: Option<LocalAudioTrack>,
}

pub struct Missing;
pub struct Present;

/// A builder for creating and connecting to a LiveKit room.
pub struct LiveKitRoomBuilder<I> {
    config: LiveKitConfig,
    identity: Option<String>,
    name: Option<String>,
    room_name: Option<String>,
    options: RoomOptions,
    grants: Option<livekit_api::access_token::VideoGrants>,
    setup_audio: bool,
    audio_sample_rate: u32,
    audio_num_channels: u32,
    _identity: PhantomData<I>,
}

impl LiveKitRoomBuilder<Missing> {
    /// Create a new builder with the given configuration.
    pub fn new(config: LiveKitConfig) -> Self {
        Self {
            config,
            identity: None,
            name: None,
            room_name: None,
            options: RoomOptions::default(),
            grants: None,
            setup_audio: false,
            audio_sample_rate: 24_000,
            audio_num_channels: 1,
            _identity: PhantomData,
        }
    }

    /// Set the required identity for the participant.
    pub fn identity(self, identity: impl Into<String>) -> LiveKitRoomBuilder<Present> {
        LiveKitRoomBuilder {
            config: self.config,
            identity: Some(identity.into()),
            name: self.name,
            room_name: self.room_name,
            options: self.options,
            grants: self.grants,
            setup_audio: self.setup_audio,
            audio_sample_rate: self.audio_sample_rate,
            audio_num_channels: self.audio_num_channels,
            _identity: PhantomData,
        }
    }
}

impl<I> LiveKitRoomBuilder<I> {
    /// Set the participant name.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set the room name to join.
    pub fn room_name(mut self, room_name: impl Into<String>) -> Self {
        self.room_name = Some(room_name.into());
        self
    }

    /// Set the room options.
    pub fn options(mut self, options: RoomOptions) -> Self {
        self.options = options;
        self
    }

    /// Set auto subscribe option.
    pub fn auto_subscribe(mut self, auto_subscribe: bool) -> Self {
        self.options.auto_subscribe = auto_subscribe;
        self
    }

    /// Set explicit video grants for the participant.
    /// If not set, standard room join grants are automatically generated.
    pub fn grants(mut self, grants: livekit_api::access_token::VideoGrants) -> Self {
        self.grants = Some(grants);
        self
    }

    /// Enable automatic setup of a local audio source and publication.
    pub fn with_audio(mut self, sample_rate: u32, num_channels: u32) -> Self {
        self.setup_audio = true;
        self.audio_sample_rate = sample_rate;
        self.audio_num_channels = num_channels;
        self
    }
}

impl LiveKitRoomBuilder<Present> {
    /// Connect to the LiveKit room and return the configured bundle.
    pub async fn connect(self) -> Result<LiveKitRoomBundle, LiveKitError> {
        let identity = self.identity.expect("identity guaranteed by builder state");

        if identity.is_empty() {
            return Err(LiveKitError::ConfigError(
                "Cannot connect to LiveKit with an empty identity".to_string(),
            ));
        }

        let room_name = match self.room_name {
            Some(name) if !name.is_empty() => name,
            Some(_) => {
                return Err(LiveKitError::ConfigError(
                    "Cannot connect to LiveKit with an empty room_name".to_string(),
                ));
            }
            None => generate_room_name("adkrust"),
        };

        let mut final_grants = self.grants.unwrap_or_default();
        final_grants.room_join = true;
        final_grants.room = room_name.clone();

        let token = self.config.generate_token_with_name(
            &identity,
            self.name.as_deref(),
            Some(final_grants),
        )?;

        tracing::info!(room_name = %room_name, identity = %identity, "connecting to livekit.room");

        let (room, events) = Room::connect(&self.config.url, &token, self.options)
            .await
            .map_err(LiveKitError::ConnectionError)?;

        tracing::info!(
            participant = %room.local_participant().identity(),
            "connected to livekit.room"
        );

        let mut audio_source = None;
        let mut audio_track = None;

        if self.setup_audio {
            let options = AudioSourceOptions {
                echo_cancellation: true,
                noise_suppression: true,
                auto_gain_control: true,
            };

            let source = NativeAudioSource::new(
                options,
                self.audio_sample_rate,
                self.audio_num_channels,
                self.audio_sample_rate / 100,
            );
            let rtc_source = RtcAudioSource::Native(source.clone());
            let track_name = format!("{identity}-audio");
            let track = LocalAudioTrack::create_audio_track(&track_name, rtc_source);

            room.local_participant()
                .publish_track(
                    livekit::prelude::LocalTrack::Audio(track.clone()),
                    livekit::options::TrackPublishOptions {
                        source: TrackSource::Microphone,
                        ..Default::default()
                    },
                )
                .await
                .map_err(LiveKitError::ConnectionError)?;

            audio_source = Some(source);
            audio_track = Some(track);
        }

        Ok(LiveKitRoomBundle { room, events, audio_source, audio_track })
    }
}

fn generate_room_name(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_livekit_builder_options() {
        let config = LiveKitConfig::new("wss://test.url", "key", "secret").unwrap();
        let grants = livekit_api::access_token::VideoGrants {
            room_join: true,
            room: "test-room".to_string(),
            ..Default::default()
        };

        let builder = LiveKitRoomBuilder::new(config)
            .identity("agent1")
            .name("Agent")
            .room_name("test-room")
            .auto_subscribe(false)
            .grants(grants.clone());

        assert_eq!(builder.identity.as_deref(), Some("agent1"));
        assert_eq!(builder.name.as_deref(), Some("Agent"));
        assert_eq!(builder.room_name.as_deref(), Some("test-room"));
        assert_eq!(builder.options.auto_subscribe, false);
        assert_eq!(builder.grants.unwrap().room, "test-room");
    }

    #[tokio::test]
    #[ignore]
    async fn test_livekit_builder_connect_integration() {
        let url = std::env::var("LIVEKIT_URL").unwrap_or_else(|_| "ws://localhost:7880".into());
        let key = std::env::var("LIVEKIT_API_KEY").unwrap_or_else(|_| "devkey".into());
        let secret = std::env::var("LIVEKIT_API_SECRET").unwrap_or_else(|_| "secret".into());

        let config = LiveKitConfig::new(url, key, secret).unwrap();
        let builder = LiveKitRoomBuilder::new(config).identity("test-agent").room_name("test-room");

        // Should fail gracefully if credentials are bad, or succeed if a local server is running.
        let _ = builder.connect().await;
    }

    #[tokio::test]
    async fn test_livekit_builder_empty_identity_connect() {
        let config = LiveKitConfig::new("wss://test.url", "key", "secret").unwrap();
        let builder = LiveKitRoomBuilder::new(config).identity("");

        let result = builder.connect().await;
        assert!(matches!(result, Err(LiveKitError::ConfigError(_))));
    }
}
