//! The windows' own background (spec 3.3 §9): what shows before a page paints, in the page's
//! light or dark `--bg`, so a dark desktop never flashes white.

/// The `--bg` token in light: the web home's (`adk-surface/web/src/styles/tokens.css`) and the
/// bundled pages' (`probe/src/pages.css`).
pub const LIGHT_BACKGROUND: [u8; 3] = [0xff, 0xff, 0xff];

/// The `--bg` token in dark.
pub const DARK_BACKGROUND: [u8; 3] = [0x0a, 0x16, 0x28];

/// The background a window takes before its page paints, as red, green and blue: the page's
/// own `--bg` in the desktop's light or dark appearance.
///
/// # Example
///
/// ```
/// use adk_desktop_core::window_background;
///
/// assert_eq!(window_background(true), [0x0a, 0x16, 0x28]);
/// assert_eq!(window_background(false), [0xff, 0xff, 0xff]);
/// ```
pub fn window_background(dark: bool) -> [u8; 3] {
    if dark { DARK_BACKGROUND } else { LIGHT_BACKGROUND }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `--bg: #rrggbb;` a style sheet declares.
    fn backgrounds(sheet: &str) -> Vec<[u8; 3]> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(sheet);
        let text = std::fs::read_to_string(&path).unwrap();
        text.lines()
            .filter_map(|line| {
                line.trim().strip_prefix("--bg: #")?.strip_suffix(';').map(str::to_owned)
            })
            .map(|hex| {
                let byte = |at: usize| u8::from_str_radix(&hex[at..at + 2], 16).unwrap();
                [byte(0), byte(2), byte(4)]
            })
            .collect()
    }

    #[test]
    fn the_window_takes_the_pages_own_background() {
        for sheet in ["../../web/src/styles/tokens.css", "../probe/src/pages.css"] {
            let mut found = backgrounds(sheet);
            found.sort_unstable();
            found.dedup();
            assert_eq!(found, [DARK_BACKGROUND, LIGHT_BACKGROUND], "{sheet}");
        }
    }
}
