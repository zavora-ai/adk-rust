//! The pairing link as a QR code (spec 3.3 §5.4): "Add a phone or app" shows it, the phone's
//! camera opens it.

use qrcode::QrCode;
use qrcode::render::svg;

use super::error::MobileError;

/// The SVG of `url`'s QR code: dark modules on white, whatever the page's theme, so every
/// camera reads it.
pub(crate) fn svg(url: &str) -> Result<String, MobileError> {
    let code = QrCode::new(url.as_bytes()).map_err(|error| {
        MobileError::Surface(crate::error::SurfaceError::Invalid(format!(
            "the link does not fit a QR code: {error}"
        )))
    })?;
    Ok(code
        .render::<svg::Color<'_>>()
        .min_dimensions(240, 240)
        .dark_color(svg::Color("#0F172A"))
        .light_color(svg::Color("#FFFFFF"))
        .build())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_draws_as_an_svg() {
        let drawn = svg("https://home.example.ts.net/ui/pair#code=ABCD-EFGH").unwrap();
        assert!(drawn.starts_with("<?xml") && drawn.contains("<svg"), "{drawn}");
        assert!(drawn.contains("#0F172A"));
    }
}
