//! Image-to-tensor preparation and the small numeric steps the backends share.

use image::imageops::{self, FilterType};

use crate::geometry::Letterbox;
use crate::manifest::IoSpec;
use crate::media::DecodedImage;

/// How an image becomes a model's `[3, height, width]` input.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Normalize {
    /// The input size, `(width, height)`.
    pub size: (u32, u32),
    /// How the image is fitted into it.
    pub letterbox: Letterbox,
    /// Subtracted from each RGB channel in `[0, 1]`.
    pub mean: [f32; 3],
    /// Each channel is divided by it.
    pub std: [f32; 3],
}

impl Normalize {
    /// Reads the input size, letterbox and normalisation from a manifest's `IoSpec`, with
    /// `size` and `letterbox` when the manifest names no input. No normalisation is
    /// `mean = 0`, `std = 1`.
    pub fn from_io(io: &IoSpec, size: (u32, u32), letterbox: Letterbox) -> Self {
        let (size, letterbox) = io
            .input
            .map_or((size, letterbox), |input| ((input.width, input.height), input.letterbox));
        Self { size, letterbox, mean: io.mean.unwrap_or([0.0; 3]), std: io.std.unwrap_or([1.0; 3]) }
    }

    /// The image as a planar RGB tensor of `3 × height × width` values: resized
    /// (bilinear, antialiased when shrinking, as PIL's `BILINEAR`) into the letterbox,
    /// rescaled to `[0, 1]` and normalised. Padding is `0` after normalisation, the mean
    /// colour. Alpha is ignored.
    pub fn tensor(&self, pixels: &DecodedImage) -> Vec<f32> {
        let (iw, ih) = (self.size.0 as usize, self.size.1 as usize);
        let geometry = self.letterbox.geometry(pixels.size(), self.size);
        let (rw, rh) = geometry.resized;
        let resized;
        let source = if (rw, rh) == pixels.size() {
            pixels.inner()
        } else {
            resized = imageops::resize(pixels.inner(), rw, rh, FilterType::Triangle);
            &resized
        };
        let mut out = vec![0.0f32; 3 * iw * ih];
        let (left, top) = (geometry.pad_left as usize, geometry.pad_top as usize);
        let scale =
            [1.0 / (255.0 * self.std[0]), 1.0 / (255.0 * self.std[1]), 1.0 / (255.0 * self.std[2])];
        let offset =
            [self.mean[0] / self.std[0], self.mean[1] / self.std[1], self.mean[2] / self.std[2]];
        for (x, y, pixel) in source.enumerate_pixels() {
            let (tx, ty) = (x as usize + left, y as usize + top);
            if tx >= iw || ty >= ih {
                continue;
            }
            for c in 0..3 {
                out[c * iw * ih + ty * iw + tx] = f32::from(pixel.0[c]) * scale[c] - offset[c];
            }
        }
        out
    }
}

/// The logistic function.
pub(crate) fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// Scales `v` to unit length; a zero vector stays zero.
pub(crate) fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        v.iter_mut().for_each(|x| *x /= norm);
    }
}

/// Bilinear resampling of a row-major `src` raster of `from` to `to`, with PyTorch's
/// `align_corners=False` convention, as SAM's mask post-processing upsamples logits.
pub(crate) fn upsample_bilinear(src: &[f32], from: (usize, usize), to: (usize, usize)) -> Vec<f32> {
    let ((sw, sh), (dw, dh)) = (from, to);
    if from == to {
        return src.to_vec();
    }
    let axis = |dst: usize, src_len: usize, dst_len: usize| {
        let scale = src_len as f32 / dst_len as f32;
        let s = ((dst as f32 + 0.5) * scale - 0.5).clamp(0.0, (src_len - 1) as f32);
        let i0 = s.floor() as usize;
        let i1 = (i0 + 1).min(src_len - 1);
        (i0, i1, s - i0 as f32)
    };
    let xs: Vec<_> = (0..dw).map(|x| axis(x, sw, dw)).collect();
    let mut out = Vec::with_capacity(dw * dh);
    for y in 0..dh {
        let (y0, y1, fy) = axis(y, sh, dh);
        let (r0, r1) = (&src[y0 * sw..(y0 + 1) * sw], &src[y1 * sw..(y1 + 1) * sw]);
        for &(x0, x1, fx) in &xs {
            let top = r0[x0] + (r0[x1] - r0[x0]) * fx;
            let bottom = r1[x0] + (r1[x1] - r1[x0]) * fx;
            out.push(top + (bottom - top) * fy);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(width: u32, height: u32, rgb: impl Fn(u32, u32) -> [u8; 3]) -> DecodedImage {
        let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
        for y in 0..height {
            for x in 0..width {
                let [r, g, b] = rgb(x, y);
                rgba.extend_from_slice(&[r, g, b, 255]);
            }
        }
        DecodedImage::from_rgba(width, height, rgba).unwrap()
    }

    #[test]
    fn an_image_at_the_input_size_is_rescaled_and_normalised_planar() {
        let pixels = image(2, 1, |x, _| if x == 0 { [255, 0, 51] } else { [0, 255, 102] });
        let n = Normalize {
            size: (2, 1),
            letterbox: Letterbox::Stretch,
            mean: [0.5; 3],
            std: [0.5; 3],
        };
        // Channel planes: R = [1, -1], G = [-1, 1], B = [0.2 * 2 - 1, 0.4 * 2 - 1].
        let t = n.tensor(&pixels);
        let expected = [1.0, -1.0, -1.0, 1.0, -0.6, -0.2];
        assert!(t.iter().zip(expected).all(|(a, b)| (a - b).abs() < 1e-6), "{t:?}");
    }

    #[test]
    fn a_centred_letterbox_pads_with_the_mean_colour() {
        // A 4x2 white image into a 4x4 input: two rows of image between one padded row
        // above and one below.
        let pixels = image(4, 2, |_, _| [255, 255, 255]);
        let n =
            Normalize { size: (4, 4), letterbox: Letterbox::Center, mean: [0.0; 3], std: [1.0; 3] };
        let t = n.tensor(&pixels);
        let red: Vec<f32> = t[..16].to_vec();
        assert_eq!(&red[..4], &[0.0; 4]);
        assert_eq!(&red[4..12], &[1.0; 8]);
        assert_eq!(&red[12..], &[0.0; 4]);
    }

    #[test]
    fn the_manifest_names_the_input_and_its_normalisation() {
        let io: IoSpec = serde_json::from_value(serde_json::json!({
            "input": {"width": 384, "height": 384, "letterbox": "stretch"},
            "mean": [0.485, 0.456, 0.406],
            "std": [0.229, 0.224, 0.225]
        }))
        .unwrap();
        let n = Normalize::from_io(&io, (1, 1), Letterbox::Center);
        assert_eq!(
            n,
            Normalize {
                size: (384, 384),
                letterbox: Letterbox::Stretch,
                mean: [0.485, 0.456, 0.406],
                std: [0.229, 0.224, 0.225],
            }
        );
        let bare = Normalize::from_io(&IoSpec::default(), (256, 256), Letterbox::Stretch);
        assert_eq!((bare.mean, bare.std), ([0.0; 3], [1.0; 3]));
    }

    #[test]
    fn bilinear_upsampling_matches_align_corners_false() {
        // torch.nn.functional.interpolate([[0, 1], [2, 3]], size=(4, 4), mode="bilinear",
        // align_corners=False)
        let up = upsample_bilinear(&[0.0, 1.0, 2.0, 3.0], (2, 2), (4, 4));
        let expected = [
            0.0, 0.25, 0.75, 1.0, //
            0.5, 0.75, 1.25, 1.5, //
            1.5, 1.75, 2.25, 2.5, //
            2.0, 2.25, 2.75, 3.0,
        ];
        assert!(up.iter().zip(expected).all(|(a, b)| (a - b).abs() < 1e-6), "{up:?}");
    }

    #[test]
    fn normalising_a_vector_gives_unit_length() {
        let mut v = [3.0, 4.0];
        l2_normalize(&mut v);
        assert_eq!(v, [0.6, 0.8]);
        let mut zero = [0.0, 0.0];
        l2_normalize(&mut zero);
        assert_eq!(zero, [0.0, 0.0]);
        assert!((sigmoid(0.0) - 0.5).abs() < f32::EPSILON);
    }
}
