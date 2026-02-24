//! Property P2: FxChain Composition
//!
//! *For any* FxChain with N stages, processing an AudioFrame SHALL apply each
//! stage in order. An empty FxChain SHALL return the input frame unchanged.
//!
//! **Validates: Requirement 5**

use adk_audio::{AudioFrame, AudioProcessor, AudioResult, FxChain};
use async_trait::async_trait;
use bytes::Bytes;
use proptest::prelude::*;

/// A processor that adds a fixed offset to all samples (for testing order).
struct OffsetProcessor {
    offset: i16,
}

#[async_trait]
impl AudioProcessor for OffsetProcessor {
    async fn process(&self, frame: &AudioFrame) -> AudioResult<AudioFrame> {
        let samples = frame.samples();
        let pcm: Vec<u8> = samples
            .iter()
            .flat_map(|&s| {
                let val = (s as i32 + self.offset as i32).clamp(-32768, 32767) as i16;
                val.to_le_bytes()
            })
            .collect();
        Ok(AudioFrame::new(Bytes::from(pcm), frame.sample_rate, frame.channels))
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// P2.1: Empty chain returns input unchanged
    #[test]
    fn prop_empty_chain_identity(
        data in proptest::collection::vec(any::<u8>(), 2..100).prop_map(|mut v| { v.truncate(v.len() & !1); v }),
    ) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let frame = AudioFrame::new(Bytes::from(data.clone()), 16000, 1);
            let chain = FxChain::new();
            let result = chain.process(&frame).await.unwrap();
            prop_assert_eq!(&result.data[..], &data[..]);
            Ok(())
        })?;
    }

    /// P2.2: Stages are applied in order
    #[test]
    fn prop_stages_in_order(n_stages in 1usize..5) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            // Start with a frame of zeros
            let frame = AudioFrame::silence(16000, 1, 10);

            // Build chain: each stage adds 1
            let mut chain = FxChain::new();
            for _ in 0..n_stages {
                chain = chain.push(OffsetProcessor { offset: 1 });
            }

            let result = chain.process(&frame).await.unwrap();
            let expected = n_stages as i16;
            for &s in result.samples() {
                prop_assert_eq!(s, expected, "expected {} after {} stages, got {}", expected, n_stages, s);
            }
            Ok(())
        })?;
    }
}
