//! Server-code refusal and content sniffing (spec 3.5 A-I1, §7.8.1 "Server code").
//!
//! A release containing a server-executable file or a server-side include directive is
//! refused, and no flag overrides it. [`scan_file`] is the one rule set the release
//! builder, the launch gate and the media pipeline share:
//!
//! | Rule | Refuses |
//! |------|---------|
//! | Extension | `.php`, `.phtml`, `.phar`, `.cgi`, `.pl`, `.py`, `.asp`, `.aspx`, `.jsp`, `.shtml` and their variants, in any position of the file name (`shell.php.jpg`) |
//! | File name | `.htaccess`, `.htpasswd`, `.user.ini`, `php.ini`, `web.config` |
//! | Directive | `<!--#include` and the other SSI directives, `<?php`, `<?=`, `<%@`, `<%=`, `<%!`, `<%--` in markup |
//! | Shebang | Any file starting with `#!` |
//! | Sniffing | ELF, PE, Mach-O and Java class files whatever their name; `<?php` and SSI `include`/`exec` inside any binary file (polyglots) |

use serde::{Deserialize, Serialize};

/// Extensions a web server may execute (A-I1), lower case, without the dot.
pub const REFUSED_EXTENSIONS: &[&str] = &[
    "php", "php3", "php4", "php5", "php7", "php8", "phtml", "pht", "phps", "phar", "cgi", "fcgi",
    "pl", "py", "asp", "aspx", "ascx", "ashx", "asmx", "axd", "cer", "jsp", "jspx", "jsw", "jsv",
    "jspf", "shtml", "shtm", "stm", "cfm", "cfml", "cfc",
];

/// File names that configure a web server to execute or rewrite (A-I1), lower case.
pub const REFUSED_FILE_NAMES: &[&str] =
    &[".htaccess", ".htpasswd", ".user.ini", "php.ini", "web.config"];

/// SSI directive names after `<!--#`.
const SSI_DIRECTIVES: &[&str] = &[
    "include", "exec", "echo", "config", "set", "if", "elif", "else", "endif", "fsize", "flastmod",
    "printenv",
];

/// Extensions served as markup that a server may parse for directives.
const MARKUP_EXTENSIONS: &[&str] = &["html", "htm", "xhtml", "xht", "xml", "svg", "svgz"];

/// Extensions of data a server serves as is: directives inside them are text, so a
/// Markdown twin quoting `<?php` in a code block is not refused.
const DATA_TEXT_EXTENSIONS: &[&str] =
    &["md", "markdown", "txt", "json", "jsonld", "css", "js", "mjs", "csv", "map", "webmanifest"];

/// Why a file was refused (A-I1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
#[non_exhaustive]
pub enum ServerCodeKind {
    /// The file name carries an executable extension.
    Extension {
        /// The extension.
        extension: String,
    },
    /// The file name is a server configuration file.
    FileName {
        /// The file name.
        name: String,
    },
    /// A server-side include directive (`<!--#include …`).
    SsiDirective {
        /// The directive.
        directive: String,
    },
    /// A PHP open tag.
    PhpTag,
    /// An ASP or JSP tag.
    AspTag,
    /// The file starts with `#!`.
    Shebang,
    /// The content is a native or JVM executable.
    Executable {
        /// The sniffed format.
        format: String,
    },
}

/// One A-I1 finding: a file and why it is refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerCodeFinding {
    /// The file's path in the release, `/`-separated, without a leading slash.
    pub path: String,
    /// Why it is refused.
    #[serde(flatten)]
    pub kind: ServerCodeKind,
    /// The byte offset of a directive or tag in the content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<usize>,
}

/// A content type recognised from a file's first bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Sniffed {
    /// PNG image.
    Png,
    /// JPEG image.
    Jpeg,
    /// GIF image.
    Gif,
    /// WebP image.
    Webp,
    /// AVIF or HEIF image.
    Avif,
    /// ICO icon.
    Ico,
    /// PDF document.
    Pdf,
    /// ZIP archive (also JAR, DOCX, EPUB).
    Zip,
    /// gzip stream.
    Gzip,
    /// MP4 or QuickTime video.
    Mp4,
    /// WebM or Matroska video.
    Webm,
    /// MP3 audio.
    Mp3,
    /// Ogg container.
    Ogg,
    /// WOFF or WOFF2 font.
    Woff,
    /// WebAssembly module.
    Wasm,
    /// ELF executable.
    Elf,
    /// Windows PE executable.
    Pe,
    /// Mach-O executable.
    MachO,
    /// Java class file.
    JavaClass,
    /// Text starting with `#!`.
    Script,
    /// SVG image (text).
    Svg,
    /// HTML (text).
    Html,
}

impl Sniffed {
    /// The media type for this format.
    pub fn content_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
            Self::Avif => "image/avif",
            Self::Ico => "image/x-icon",
            Self::Pdf => "application/pdf",
            Self::Zip => "application/zip",
            Self::Gzip => "application/gzip",
            Self::Mp4 => "video/mp4",
            Self::Webm => "video/webm",
            Self::Mp3 => "audio/mpeg",
            Self::Ogg => "audio/ogg",
            Self::Woff => "font/woff2",
            Self::Wasm => "application/wasm",
            Self::Elf | Self::Pe | Self::MachO | Self::JavaClass => "application/octet-stream",
            Self::Script => "text/x-script",
            Self::Svg => "image/svg+xml",
            Self::Html => "text/html",
        }
    }

    /// Whether the format is a native or JVM executable.
    pub fn is_executable(self) -> bool {
        matches!(self, Self::Elf | Self::Pe | Self::MachO | Self::JavaClass | Self::Script)
    }

    /// Whether the format is a raster image the media pipeline can re-encode.
    pub fn is_raster_image(self) -> bool {
        matches!(self, Self::Png | Self::Jpeg | Self::Gif | Self::Webp)
    }
}

/// Recognises a file's format from its first bytes.
///
/// # Example
///
/// ```rust
/// use adk_site::scan::{Sniffed, sniff};
///
/// assert_eq!(sniff(b"\x89PNG\r\n\x1a\n...."), Some(Sniffed::Png));
/// assert_eq!(sniff(b"\x7fELF\x02\x01"), Some(Sniffed::Elf));
/// assert_eq!(sniff(b"plain text"), None);
/// ```
pub fn sniff(bytes: &[u8]) -> Option<Sniffed> {
    let starts = |magic: &[u8]| bytes.starts_with(magic);
    if starts(b"\x89PNG\r\n\x1a\n") {
        return Some(Sniffed::Png);
    }
    if starts(b"\xff\xd8\xff") {
        return Some(Sniffed::Jpeg);
    }
    if starts(b"GIF87a") || starts(b"GIF89a") {
        return Some(Sniffed::Gif);
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some(Sniffed::Webp);
    }
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        let brand = &bytes[8..12];
        return Some(match brand {
            b"avif" | b"avis" | b"heic" | b"heix" | b"mif1" | b"msf1" => Sniffed::Avif,
            _ => Sniffed::Mp4,
        });
    }
    if starts(b"\x00\x00\x01\x00") {
        return Some(Sniffed::Ico);
    }
    if starts(b"%PDF-") {
        return Some(Sniffed::Pdf);
    }
    if starts(b"PK\x03\x04") || starts(b"PK\x05\x06") {
        return Some(Sniffed::Zip);
    }
    if starts(b"\x1f\x8b") {
        return Some(Sniffed::Gzip);
    }
    if starts(b"\x1a\x45\xdf\xa3") {
        return Some(Sniffed::Webm);
    }
    if starts(b"ID3") || (bytes.len() >= 2 && bytes[0] == 0xff && bytes[1] & 0xe0 == 0xe0) {
        return Some(Sniffed::Mp3);
    }
    if starts(b"OggS") {
        return Some(Sniffed::Ogg);
    }
    if starts(b"wOFF") || starts(b"wOF2") {
        return Some(Sniffed::Woff);
    }
    if starts(b"\x00asm") {
        return Some(Sniffed::Wasm);
    }
    if starts(b"\x7fELF") {
        return Some(Sniffed::Elf);
    }
    if starts(b"MZ") {
        return Some(Sniffed::Pe);
    }
    if starts(b"\xfe\xed\xfa\xce")
        || starts(b"\xfe\xed\xfa\xcf")
        || starts(b"\xce\xfa\xed\xfe")
        || starts(b"\xcf\xfa\xed\xfe")
    {
        return Some(Sniffed::MachO);
    }
    // 0xCAFEBABE is also a Mach-O universal binary; both are refused.
    if starts(b"\xca\xfe\xba\xbe") {
        return Some(Sniffed::JavaClass);
    }
    if starts(b"#!") {
        return Some(Sniffed::Script);
    }
    let head = &bytes[..bytes.len().min(512)];
    let text = String::from_utf8_lossy(head).trim_start_matches('\u{feff}').to_ascii_lowercase();
    let text = text.trim_start();
    if text.starts_with("<svg") || (text.starts_with("<?xml") && text.contains("<svg")) {
        return Some(Sniffed::Svg);
    }
    if text.starts_with("<!doctype html") || text.starts_with("<html") {
        return Some(Sniffed::Html);
    }
    None
}

/// The lower-case extensions of a file name after its first dot: `a.php.jpg` gives
/// `["php", "jpg"]`. A leading dot is part of the name, not an extension.
fn name_extensions(name: &str) -> Vec<String> {
    let trimmed = name.trim_start_matches('.');
    trimmed.split('.').skip(1).map(str::to_ascii_lowercase).collect()
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn find_ci(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (from..=haystack.len() - needle.len())
        .find(|&i| haystack[i..i + needle.len()].eq_ignore_ascii_case(needle))
}

/// Finds SSI directives (`<!--#name`) in `bytes`; `only` restricts the names.
fn ssi_directives(bytes: &[u8], only: Option<&[&str]>) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = find_ci(bytes, b"<!--#", from) {
        let rest = &bytes[at + 5..];
        let name: String = rest
            .iter()
            .skip_while(|b| b.is_ascii_whitespace())
            .take_while(|b| b.is_ascii_alphabetic())
            .map(|b| (*b as char).to_ascii_lowercase())
            .collect();
        let listed = only.unwrap_or(SSI_DIRECTIVES);
        if listed.contains(&name.as_str()) {
            found.push((at, name));
        }
        from = at + 5;
    }
    found
}

/// Scans one release file for server code (A-I1) and returns every finding; an empty
/// list means the file may ship.
///
/// # Example
///
/// ```rust
/// use adk_site::scan::{ServerCodeKind, scan_file};
///
/// assert!(scan_file("index.html", b"<!doctype html><p>hello</p>").is_empty());
/// let findings = scan_file("index.html", b"<p><!--#include virtual=\"/x\" --></p>");
/// assert!(matches!(findings[0].kind, ServerCodeKind::SsiDirective { .. }));
/// assert!(!scan_file("uploads/shell.php.jpg", b"").is_empty());
/// ```
pub fn scan_file(path: &str, bytes: &[u8]) -> Vec<ServerCodeFinding> {
    let mut findings = Vec::new();
    let mut push = |kind: ServerCodeKind, offset: Option<usize>| {
        findings.push(ServerCodeFinding { path: path.to_string(), kind, offset });
    };
    let name = file_name(path);
    let lower_name = name.to_ascii_lowercase();
    if REFUSED_FILE_NAMES.contains(&lower_name.as_str()) {
        push(ServerCodeKind::FileName { name: name.to_string() }, None);
    }
    let extensions = name_extensions(name);
    for extension in &extensions {
        if REFUSED_EXTENSIONS.contains(&extension.as_str()) {
            push(ServerCodeKind::Extension { extension: extension.clone() }, None);
        }
    }
    let sniffed = sniff(bytes);
    match sniffed {
        Some(Sniffed::Script) => push(ServerCodeKind::Shebang, Some(0)),
        Some(format @ (Sniffed::Elf | Sniffed::Pe | Sniffed::MachO | Sniffed::JavaClass)) => push(
            ServerCodeKind::Executable { format: format!("{format:?}").to_lowercase() },
            Some(0),
        ),
        _ => {}
    }
    let last_extension = extensions.last().map(String::as_str).unwrap_or("");
    let is_text = std::str::from_utf8(bytes).is_ok();
    let is_markup = MARKUP_EXTENSIONS.contains(&last_extension)
        || matches!(sniffed, Some(Sniffed::Html | Sniffed::Svg));
    let is_data_text = DATA_TEXT_EXTENSIONS.contains(&last_extension) && !is_markup;
    if is_markup || (is_text && !is_data_text) {
        for (at, directive) in ssi_directives(bytes, None) {
            push(ServerCodeKind::SsiDirective { directive }, Some(at));
        }
        for tag in [b"<?php".as_slice(), b"<?="] {
            if let Some(at) = find_ci(bytes, tag, 0) {
                push(ServerCodeKind::PhpTag, Some(at));
            }
        }
        for tag in [b"<%@".as_slice(), b"<%=", b"<%!", b"<%--"] {
            if let Some(at) = find_ci(bytes, tag, 0) {
                push(ServerCodeKind::AspTag, Some(at));
                break;
            }
        }
    } else if !is_text {
        // Binary files: only markers too long to occur by chance, so an image with a
        // webshell in its comment block (a polyglot) is caught without false positives.
        if let Some(at) = find_ci(bytes, b"<?php", 0) {
            push(ServerCodeKind::PhpTag, Some(at));
        }
        for (at, directive) in ssi_directives(bytes, Some(&["include", "exec"])) {
            push(ServerCodeKind::SsiDirective { directive }, Some(at));
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_extension_is_refused() {
        for extension in REFUSED_EXTENSIONS {
            let findings = scan_file(&format!("a/b/file.{extension}"), b"");
            assert_eq!(findings.len(), 1, "{extension}");
            let upper = scan_file(&format!("file.{}", extension.to_uppercase()), b"");
            assert_eq!(upper.len(), 1, "{extension} upper case");
        }
    }

    #[test]
    fn double_extensions_and_config_files_are_refused() {
        assert!(!scan_file("uploads/shell.php.jpg", b"\xff\xd8\xff").is_empty());
        assert!(!scan_file(".htaccess", b"Options +Includes").is_empty());
        assert!(!scan_file("sub/.HTACCESS", b"").is_empty());
        assert!(!scan_file("web.config", b"<configuration/>").is_empty());
        assert!(scan_file(".well-known/security.txt", b"Contact: mailto:a@b.c").is_empty());
    }

    #[test]
    fn directives_in_markup_are_refused_and_quoted_text_is_not() {
        let html = b"<html><body><!--#exec cmd=\"id\" --></body></html>";
        assert_eq!(
            scan_file("index.html", html)[0].kind,
            ServerCodeKind::SsiDirective { directive: "exec".into() }
        );
        assert_eq!(scan_file("p.htm", b"<p><?php echo 1; ?></p>")[0].kind, ServerCodeKind::PhpTag);
        assert_eq!(scan_file("p.html", b"<%@ page %>")[0].kind, ServerCodeKind::AspTag);
        // Escaped in HTML, verbatim in a Markdown twin or a JSON machine view: inert data.
        assert!(scan_file("p.html", b"<code>&lt;?php echo 1; ?&gt;</code>").is_empty());
        assert!(scan_file("p/index.md", b"```php\n<?php echo 1;\n```\n").is_empty());
        assert!(scan_file("p/index.awp.json", br#"{"body":"<?php"}"#).is_empty());
        // An XML declaration is not a PHP tag.
        assert!(scan_file("sitemap.xml", b"<?xml version=\"1.0\"?><urlset/>").is_empty());
        // A comment is not a directive.
        assert!(scan_file("a.html", b"<!-- # not a directive -->").is_empty());
    }

    #[test]
    fn executables_are_sniffed_whatever_their_name() {
        for (bytes, format) in [
            (b"\x7fELF\x02\x01\x01".as_slice(), "elf"),
            (b"MZ\x90\x00", "pe"),
            (b"\xcf\xfa\xed\xfe", "macho"),
            (b"\xca\xfe\xba\xbe", "javaclass"),
        ] {
            let findings = scan_file("images/logo.png", bytes);
            assert_eq!(findings[0].kind, ServerCodeKind::Executable { format: format.into() });
        }
        assert_eq!(scan_file("notes.txt", b"#!/bin/sh\nid\n")[0].kind, ServerCodeKind::Shebang);
    }

    #[test]
    fn polyglot_images_are_refused() {
        let mut jpeg = b"\xff\xd8\xff\xe0\x00\x10JFIF\x00".to_vec();
        jpeg.extend_from_slice(b"\xff\xfe\x00\x20<?php system($_GET['c']); ?>");
        jpeg.extend_from_slice(&[0x80; 64]);
        assert_eq!(scan_file("uploads/photo.jpg", &jpeg)[0].kind, ServerCodeKind::PhpTag);
        let clean = [0xff, 0xd8, 0xff, 0xe0, 0x3c, 0x25, 0x40, 0x80, 0x3c, 0x3f, 0x3d];
        assert!(scan_file("uploads/photo.jpg", &clean).is_empty(), "short markers in binary");
    }
}
