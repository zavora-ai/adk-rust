//! Pointer, keyboard and capture primitives over a WebDriver viewport, in the pixel space of
//! the screenshots the viewport returns. Shared by the browser use toolset and the WebDriver
//! computer actuator.

use crate::session::BrowserSession;
use adk_core::{AdkError, MouseButton, Result, ScreenPoint, Screenshot};
use serde_json::{Value, json};
use std::io::Cursor;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use thirtyfour::WebDriver;

/// The long-edge and visual-token limits a screenshot must fit for Claude Opus 4.7 and later.
const MAX_LONG_EDGE: u32 = 2_576;
const MAX_TILES: u64 = 4_784;

/// Marks every script so a recorded WebDriver fixture can recognise it.
pub(crate) fn script(name: &str, body: &str) -> String {
    format!("/*adk-browser:{name}*/\n{body}")
}

const DPR_SCRIPT: &str =
    "return [window.devicePixelRatio || 1, window.innerWidth, window.innerHeight];";

const SCROLL_SCRIPT: &str = r#"
const [x, y, dx, dy] = arguments;
let el = document.elementFromPoint(x, y);
while (el && el !== document.body && el !== document.documentElement) {
  const style = getComputedStyle(el);
  const scrollY = /(auto|scroll)/.test(style.overflowY) && el.scrollHeight > el.clientHeight;
  const scrollX = /(auto|scroll)/.test(style.overflowX) && el.scrollWidth > el.clientWidth;
  if ((dy && scrollY) || (dx && scrollX)) { el.scrollBy(dx, dy); return true; }
  el = el.parentElement;
}
window.scrollBy(dx, dy);
return true;
"#;

const MIDDLE_CLICK_SCRIPT: &str = r#"
const [x, y] = arguments;
const el = document.elementFromPoint(x, y);
if (!el) return false;
for (const type of ['mousedown', 'mouseup', 'auxclick']) {
  el.dispatchEvent(new MouseEvent(type, { bubbles: true, cancelable: true, button: 1, clientX: x, clientY: y, view: window }));
}
return true;
"#;

/// The WebDriver key for a named key, in the xdotool (`Return`, `BackSpace`, `Page_Down`)
/// or DOM (`Enter`, `ArrowUp`) spelling, case-insensitively.
pub(crate) fn key_code(name: &str) -> Option<char> {
    let mut chars = name.chars();
    if let (Some(single), None) = (chars.next(), chars.next()) {
        return Some(single);
    }
    let code = match name.to_ascii_lowercase().replace(['_', '-'], "").as_str() {
        "return" | "enter" | "kpenter" => '\u{E007}',
        "tab" => '\u{E004}',
        "escape" | "esc" => '\u{E00C}',
        "backspace" => '\u{E003}',
        "delete" | "del" => '\u{E017}',
        "insert" => '\u{E016}',
        "space" => ' ',
        "up" | "arrowup" => '\u{E013}',
        "down" | "arrowdown" => '\u{E015}',
        "left" | "arrowleft" => '\u{E012}',
        "right" | "arrowright" => '\u{E014}',
        "home" => '\u{E011}',
        "end" => '\u{E010}',
        "pageup" | "prior" => '\u{E00E}',
        "pagedown" | "next" => '\u{E00F}',
        "shift" | "shiftl" | "shiftr" => '\u{E008}',
        "ctrl" | "control" | "controll" | "controlr" => '\u{E009}',
        "alt" | "altl" | "altr" | "option" => '\u{E00A}',
        "super" | "meta" | "cmd" | "command" | "win" | "superl" | "superr" => '\u{E03D}',
        "f1" => '\u{E031}',
        "f2" => '\u{E032}',
        "f3" => '\u{E033}',
        "f4" => '\u{E034}',
        "f5" => '\u{E035}',
        "f6" => '\u{E036}',
        "f7" => '\u{E037}',
        "f8" => '\u{E038}',
        "f9" => '\u{E039}',
        "f10" => '\u{E03A}',
        "f11" => '\u{E03B}',
        "f12" => '\u{E03C}',
        "minus" => '-',
        "plus" => '+',
        "period" => '.',
        "comma" => ',',
        "slash" => '/',
        _ => return None,
    };
    Some(code)
}

/// Parses a `+`-joined chord such as `ctrl+shift+a` into its keys, modifiers first.
pub(crate) fn chord(keys: &str) -> Result<Vec<char>> {
    let parts: Vec<&str> = keys.split('+').map(str::trim).collect();
    // A chord ending in `+` names the plus key itself.
    let parts: Vec<&str> = if keys.ends_with("++") || keys == "+" {
        let mut parts: Vec<&str> =
            parts.into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>();
        parts.push("+");
        parts
    } else {
        parts
    };
    parts
        .iter()
        .map(|part| {
            key_code(part).ok_or_else(|| {
                AdkError::tool(format!(
                    "Error: unknown key '{part}' in '{keys}'. Use names such as Return, Tab, \
                     Escape, BackSpace, Up, Page_Down, F5, or a single character, joined with + \
                     for a chord (ctrl+a)."
                ))
            })
        })
        .collect()
}

/// Modifier key codes for names such as `shift` or `ctrl`.
pub(crate) fn modifiers(names: &[String]) -> Result<Vec<char>> {
    names.iter().map(|name| chord(name).map(|keys| keys[0])).collect()
}

/// The coordinate frame of the viewport: screenshots are `screenshot_size` pixels and
/// `css_per_pixel` CSS pixels each.
#[derive(Debug, Clone, Copy)]
struct Frame {
    css_per_pixel: f64,
    device_pixel_ratio: f64,
    screenshot_size: (u32, u32),
}

/// A WebDriver viewport addressed in screenshot pixels.
pub(crate) struct Viewport {
    session: Arc<BrowserSession>,
    frame: Mutex<Option<Frame>>,
    cursor: Mutex<ScreenPoint>,
}

fn webdriver_error(action: &str, error: impl std::fmt::Display) -> AdkError {
    AdkError::tool(format!("Error: {action} failed: {error}"))
}

impl Viewport {
    pub(crate) fn new(session: Arc<BrowserSession>) -> Self {
        Self { session, frame: Mutex::new(None), cursor: Mutex::new(ScreenPoint::new(0, 0)) }
    }

    pub(crate) fn session(&self) -> &Arc<BrowserSession> {
        &self.session
    }

    pub(crate) async fn driver(&self) -> Result<WebDriver> {
        self.session.driver().await
    }

    /// Runs a marked script and returns its JSON result.
    pub(crate) async fn run(&self, name: &str, body: &str, args: Vec<Value>) -> Result<Value> {
        let driver = self.driver().await?;
        let result = driver
            .execute(script(name, body), args)
            .await
            .map_err(|error| webdriver_error(name, error))?;
        Ok(result.json().clone())
    }

    /// The current frame, measuring the viewport when no screenshot has set it.
    async fn frame(&self) -> Result<Frame> {
        if let Some(frame) = *self.frame.lock().unwrap_or_else(|e| e.into_inner()) {
            return Ok(frame);
        }
        let measured = self.run("viewport", DPR_SCRIPT, Vec::new()).await?;
        let dpr = measured[0].as_f64().filter(|dpr| *dpr > 0.0).unwrap_or(1.0);
        let width = measured[1].as_f64().unwrap_or(0.0);
        let height = measured[2].as_f64().unwrap_or(0.0);
        let size = ((width * dpr).round() as u32, (height * dpr).round() as u32);
        let (width, height) = fit(size.0, size.1);
        let frame = Frame {
            css_per_pixel: if width == 0 {
                1.0 / dpr
            } else {
                f64::from(size.0) / dpr / f64::from(width)
            },
            device_pixel_ratio: dpr,
            screenshot_size: (width, height),
        };
        Ok(frame)
    }

    /// A screenshot point in CSS pixels.
    pub(crate) async fn screen_to_css(&self, point: ScreenPoint) -> Result<(i64, i64)> {
        let frame = self.frame().await?;
        let (width, height) = frame.screenshot_size;
        if width > 0
            && (point.x < 0
                || point.y < 0
                || point.x >= i64::from(width)
                || point.y >= i64::from(height))
        {
            return Err(AdkError::tool(format!(
                "Error: coordinate ({}, {}) is outside the {width}x{height} viewport",
                point.x, point.y
            )));
        }
        Ok((
            (point.x as f64 * frame.css_per_pixel).round() as i64,
            (point.y as f64 * frame.css_per_pixel).round() as i64,
        ))
    }

    /// A CSS point in screenshot pixels.
    pub(crate) async fn css_to_screen(&self, x: f64, y: f64) -> Result<ScreenPoint> {
        let frame = self.frame().await?;
        Ok(ScreenPoint::new(
            (x / frame.css_per_pixel).round() as i64,
            (y / frame.css_per_pixel).round() as i64,
        ))
    }

    /// Captures the viewport, resized to fit the image limits, and records its frame.
    pub(crate) async fn screenshot(&self) -> Result<Screenshot> {
        let (shot, frame) = self.capture().await?;
        let (width, height) = frame.screenshot_size;
        let shot = if (shot.width, shot.height) == (width, height) {
            shot
        } else {
            let raw = decode(&shot.data)?;
            encode(&image::DynamicImage::ImageRgba8(image::imageops::resize(
                &raw,
                width,
                height,
                image::imageops::FilterType::Triangle,
            )))?
        };
        *self.frame.lock().unwrap_or_else(|e| e.into_inner()) = Some(frame);
        Ok(shot)
    }

    /// A region `[x0, y0, x1, y1]` of the viewport in screenshot pixels, captured at full
    /// resolution and scaled to fit the screenshot size.
    pub(crate) async fn zoom(&self, region: [i64; 4]) -> Result<Screenshot> {
        let (shot, frame) = self.capture().await?;
        let raw = decode(&shot.data)?;
        let scale = frame.css_per_pixel * frame.device_pixel_ratio;
        let clamp = |value: i64, max: u32| ((value.max(0) as f64 * scale).round() as u32).min(max);
        let (x0, y0) = (clamp(region[0], raw.width()), clamp(region[1], raw.height()));
        let (x1, y1) = (clamp(region[2], raw.width()), clamp(region[3], raw.height()));
        if x1 <= x0 || y1 <= y0 {
            return Err(AdkError::tool(format!(
                "Error: zoom region {region:?} is empty or outside the viewport"
            )));
        }
        let crop = image::imageops::crop_imm(&raw, x0, y0, x1 - x0, y1 - y0).to_image();
        let (max_width, max_height) = frame.screenshot_size;
        let ratio = (f64::from(max_width) / f64::from(crop.width()))
            .min(f64::from(max_height) / f64::from(crop.height()));
        let (width, height) = (
            ((f64::from(crop.width()) * ratio).floor() as u32).max(1),
            ((f64::from(crop.height()) * ratio).floor() as u32).max(1),
        );
        encode(&image::DynamicImage::ImageRgba8(image::imageops::resize(
            &crop,
            width,
            height,
            image::imageops::FilterType::CatmullRom,
        )))
    }

    /// The viewport as WebDriver captures it, and the frame screenshots of it take.
    async fn capture(&self) -> Result<(Screenshot, Frame)> {
        let driver = self.driver().await?;
        let png = driver
            .screenshot_as_png()
            .await
            .map_err(|error| webdriver_error("screenshot", error))?;
        let shot = Screenshot::png(png).ok_or_else(|| {
            AdkError::tool("Error: the browser returned a screenshot that is not a PNG")
        })?;
        let measured = self.run("viewport", DPR_SCRIPT, Vec::new()).await?;
        let dpr = measured[0].as_f64().filter(|dpr| *dpr > 0.0).unwrap_or(1.0);
        let (width, height) = fit(shot.width, shot.height);
        let css_width = f64::from(shot.width) / dpr;
        let frame = Frame {
            css_per_pixel: css_width / f64::from(width.max(1)),
            device_pixel_ratio: dpr,
            screenshot_size: (width, height),
        };
        Ok((shot, frame))
    }

    pub(crate) fn cursor(&self) -> ScreenPoint {
        *self.cursor.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set_cursor(&self, point: ScreenPoint) {
        *self.cursor.lock().unwrap_or_else(|e| e.into_inner()) = point;
    }

    /// Clicks `count` times with `button` at `at` (or the cursor), holding `modifiers`.
    pub(crate) async fn click(
        &self,
        at: Option<ScreenPoint>,
        button: MouseButton,
        count: u8,
        modifiers: &[char],
    ) -> Result<()> {
        let point = at.unwrap_or_else(|| self.cursor());
        let (x, y) = self.screen_to_css(point).await?;
        let driver = self.driver().await?;
        if button == MouseButton::Middle {
            self.run("middle_click", MIDDLE_CLICK_SCRIPT, vec![json!(x), json!(y)]).await?;
            self.set_cursor(point);
            return Ok(());
        }
        let mut chain = driver.action_chain();
        for key in modifiers {
            chain = chain.key_down(*key);
        }
        chain = chain.move_to(x, y);
        chain = match (button, count) {
            (MouseButton::Right, _) => chain.context_click(),
            (_, 2) => chain.double_click(),
            (_, 3) => chain.click().click().click(),
            _ => chain.click(),
        };
        for key in modifiers.iter().rev() {
            chain = chain.key_up(*key);
        }
        chain.perform().await.map_err(|error| webdriver_error("click", error))?;
        self.set_cursor(point);
        Ok(())
    }

    /// Moves the pointer to `at`.
    pub(crate) async fn move_to(&self, at: ScreenPoint) -> Result<()> {
        let (x, y) = self.screen_to_css(at).await?;
        let driver = self.driver().await?;
        driver
            .action_chain()
            .move_to(x, y)
            .perform()
            .await
            .map_err(|error| webdriver_error("mouse move", error))?;
        self.set_cursor(at);
        Ok(())
    }

    /// Presses or releases the left button at `at` (or the cursor).
    pub(crate) async fn button(&self, at: Option<ScreenPoint>, down: bool) -> Result<()> {
        let point = at.unwrap_or_else(|| self.cursor());
        let (x, y) = self.screen_to_css(point).await?;
        let driver = self.driver().await?;
        let chain = driver.action_chain().move_to(x, y);
        let chain = if down { chain.click_and_hold() } else { chain.release() };
        chain.perform().await.map_err(|error| webdriver_error("mouse button", error))?;
        self.set_cursor(point);
        Ok(())
    }

    /// Presses at `from`, drags to `to`, releases.
    pub(crate) async fn drag(
        &self,
        from: ScreenPoint,
        to: ScreenPoint,
        modifiers: &[char],
    ) -> Result<()> {
        let (x0, y0) = self.screen_to_css(from).await?;
        let (x1, y1) = self.screen_to_css(to).await?;
        let driver = self.driver().await?;
        let mut chain = driver.action_chain();
        for key in modifiers {
            chain = chain.key_down(*key);
        }
        chain = chain.move_to(x0, y0).click_and_hold().move_to(x1, y1).release();
        for key in modifiers.iter().rev() {
            chain = chain.key_up(*key);
        }
        chain.perform().await.map_err(|error| webdriver_error("drag", error))?;
        self.set_cursor(to);
        Ok(())
    }

    /// Scrolls `notches` wheel notches at `at` (or the cursor).
    pub(crate) async fn scroll(
        &self,
        at: Option<ScreenPoint>,
        direction: adk_core::ScrollDirection,
        notches: u32,
    ) -> Result<()> {
        let point = at.unwrap_or_else(|| self.cursor());
        let (x, y) = self.screen_to_css(point).await?;
        let step = 100 * i64::from(notches.clamp(1, 100));
        let (dx, dy) = match direction {
            adk_core::ScrollDirection::Up => (0, -step),
            adk_core::ScrollDirection::Down => (0, step),
            adk_core::ScrollDirection::Left => (-step, 0),
            adk_core::ScrollDirection::Right => (step, 0),
        };
        self.run("scroll", SCROLL_SCRIPT, vec![json!(x), json!(y), json!(dx), json!(dy)]).await?;
        self.set_cursor(point);
        Ok(())
    }

    /// Types literal text at the focus.
    pub(crate) async fn type_text(&self, text: &str) -> Result<()> {
        let driver = self.driver().await?;
        driver
            .action_chain()
            .send_keys(text)
            .perform()
            .await
            .map_err(|error| webdriver_error("typing", error))
    }

    /// Presses each space-separated chord in `keys`, `repeat` times.
    pub(crate) async fn press(&self, keys: &str, repeat: u32) -> Result<()> {
        let chords: Vec<Vec<char>> = if keys.trim() == "" {
            vec![vec![' ']]
        } else {
            keys.split_whitespace().map(chord).collect::<Result<_>>()?
        };
        let driver = self.driver().await?;
        let mut chain = driver.action_chain();
        for _ in 0..repeat.clamp(1, 100) {
            for chord in &chords {
                for key in chord {
                    chain = chain.key_down(*key);
                }
                for key in chord.iter().rev() {
                    chain = chain.key_up(*key);
                }
            }
        }
        chain.perform().await.map_err(|error| webdriver_error("key press", error))
    }

    /// Holds a chord down for `duration`.
    pub(crate) async fn hold(&self, keys: &str, duration: Duration) -> Result<()> {
        let chord = chord(keys)?;
        let driver = self.driver().await?;
        let mut down = driver.action_chain();
        for key in &chord {
            down = down.key_down(*key);
        }
        down.perform().await.map_err(|error| webdriver_error("key hold", error))?;
        tokio::time::sleep(duration).await;
        let mut up = driver.action_chain();
        for key in chord.iter().rev() {
            up = up.key_up(*key);
        }
        up.perform().await.map_err(|error| webdriver_error("key release", error))
    }

    /// Forgets the frame, so the next action measures the viewport again.
    pub(crate) fn reset_frame(&self) {
        *self.frame.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// The size a `width` × `height` image takes to fit the image limits, aspect ratio kept.
pub(crate) fn fit(width: u32, height: u32) -> (u32, u32) {
    if width == 0 || height == 0 {
        return (width, height);
    }
    let fits = |w: u32, h: u32| {
        w.max(h) <= MAX_LONG_EDGE
            && u64::from(w.div_ceil(28)) * u64::from(h.div_ceil(28)) <= MAX_TILES
    };
    if fits(width, height) {
        return (width, height);
    }
    let mut scale = f64::from(MAX_LONG_EDGE) / f64::from(width.max(height));
    loop {
        let w = ((f64::from(width) * scale).floor() as u32).max(1);
        let h = ((f64::from(height) * scale).floor() as u32).max(1);
        if fits(w, h) {
            return (w, h);
        }
        scale *= 0.98;
    }
}

fn decode(png: &[u8]) -> Result<image::RgbaImage> {
    Ok(image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .map_err(|error| webdriver_error("decoding the screenshot", error))?
        .to_rgba8())
}

fn encode(image: &image::DynamicImage) -> Result<Screenshot> {
    let mut png = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|error| webdriver_error("encoding the screenshot", error))?;
    Screenshot::png(png).ok_or_else(|| AdkError::tool("Error: the screenshot is not a PNG"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords_parse_both_spellings() {
        assert_eq!(chord("ctrl+a").unwrap(), vec!['\u{E009}', 'a']);
        assert_eq!(chord("Return").unwrap(), vec!['\u{E007}']);
        assert_eq!(chord("Enter").unwrap(), vec!['\u{E007}']);
        assert_eq!(chord("Page_Down").unwrap(), vec!['\u{E00F}']);
        assert_eq!(chord("alt+Tab").unwrap(), vec!['\u{E00A}', '\u{E004}']);
        assert_eq!(chord("ctrl++").unwrap(), vec!['\u{E009}', '+']);
        assert!(chord("hyper+x").is_err());
    }

    #[test]
    fn large_viewports_are_fitted_to_the_limits() {
        assert_eq!(fit(1280, 800), (1280, 800));
        assert_eq!(fit(1920, 1080), (1920, 1080));
        let (width, height) = fit(3024, 1964);
        assert!(width <= 2_576);
        assert!(u64::from(width.div_ceil(28)) * u64::from(height.div_ceil(28)) <= 4_784);
        assert!((f64::from(width) / f64::from(height) - 3024.0 / 1964.0).abs() < 0.01);
    }
}
