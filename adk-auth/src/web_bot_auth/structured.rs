//! Structured Field Values for HTTP ([RFC 8941]), as HTTP Message Signatures uses them.
//!
//! `Signature-Input`, `Signature` and `Signature-Agent` are Dictionaries; the covered
//! components of a signature are an Inner List of Strings with Parameters. This module parses
//! and strictly serialises every RFC 8941 type: Items (Integer, Decimal, String, Token, Byte
//! Sequence, Boolean), Parameters, Inner Lists, Lists and Dictionaries. Strict serialisation is
//! what RFC 9421 §2.1.1 and §2.1.2 put into a signature base. Spec 3.5 §7.3.
//!
//! [RFC 8941]: https://www.rfc-editor.org/rfc/rfc8941
//!
//! # Example
//!
//! ```rust
//! use adk_auth::web_bot_auth::structured::{BareItem, Dictionary, ListEntry};
//!
//! let dict = Dictionary::parse(r#"agent2="https://signature-agent.test";type=directory"#).unwrap();
//! let Some(ListEntry::Item(member)) = dict.get("agent2") else { panic!("member") };
//! assert_eq!(member.bare_item, BareItem::String("https://signature-agent.test".into()));
//! assert_eq!(dict.serialize().unwrap(), r#"agent2="https://signature-agent.test";type=directory"#);
//! ```

use base64::Engine;
use base64::alphabet;
use base64::engine::general_purpose::STANDARD;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};

/// RFC 8941 §4.2.7: recipients accept missing padding and non-zero trailing bits.
const LENIENT_BASE64: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true),
);

const MAX_INTEGER: i64 = 999_999_999_999_999;
const MAX_DECIMAL_INTEGER_PART: i64 = 999_999_999_999;

/// A value that cannot be parsed or serialised as a structured field (RFC 8941 §4).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid structured field value: {0}")]
pub struct StructuredFieldError(String);

impl StructuredFieldError {
    fn new(reason: impl Into<String>) -> Self {
        Self(reason.into())
    }
}

type Result<T> = std::result::Result<T, StructuredFieldError>;

/// An RFC 8941 Decimal, held exactly as a count of thousandths.
///
/// Decimals have at most twelve integer digits and three fractional digits (RFC 8941 §3.3.2),
/// so thousandths represent every value without rounding.
///
/// # Example
///
/// ```rust
/// use adk_auth::web_bot_auth::structured::Decimal;
///
/// let half = Decimal::from_thousandths(1_500).unwrap();
/// assert_eq!(half.thousandths(), 1_500);
/// assert!(Decimal::from_thousandths(1_000_000_000_000_000).is_none());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Decimal(i64);

impl Decimal {
    /// Creates a Decimal from thousandths, or `None` when its integer part has more than twelve
    /// digits.
    pub fn from_thousandths(thousandths: i64) -> Option<Self> {
        (thousandths.unsigned_abs() / 1000 <= MAX_DECIMAL_INTEGER_PART as u64)
            .then_some(Self(thousandths))
    }

    /// The value in thousandths.
    pub fn thousandths(self) -> i64 {
        self.0
    }
}

/// An RFC 8941 bare item (§3.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BareItem {
    /// An Integer in the range ±999,999,999,999,999.
    Integer(i64),
    /// A Decimal.
    Decimal(Decimal),
    /// A String of printable ASCII.
    String(String),
    /// A Token.
    Token(String),
    /// A Byte Sequence.
    ByteSequence(Vec<u8>),
    /// A Boolean.
    Boolean(bool),
}

impl BareItem {
    /// The String value, when this is a String.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            Self::Integer(_)
            | Self::Decimal(_)
            | Self::Token(_)
            | Self::ByteSequence(_)
            | Self::Boolean(_) => None,
        }
    }

    /// The Token value, when this is a Token.
    pub fn as_token(&self) -> Option<&str> {
        match self {
            Self::Token(value) => Some(value),
            Self::Integer(_)
            | Self::Decimal(_)
            | Self::String(_)
            | Self::ByteSequence(_)
            | Self::Boolean(_) => None,
        }
    }

    /// The Integer value, when this is an Integer.
    pub fn as_integer(&self) -> Option<i64> {
        match self {
            Self::Integer(value) => Some(*value),
            Self::Decimal(_)
            | Self::String(_)
            | Self::Token(_)
            | Self::ByteSequence(_)
            | Self::Boolean(_) => None,
        }
    }

    /// The Byte Sequence value, when this is a Byte Sequence.
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::ByteSequence(value) => Some(value),
            Self::Integer(_)
            | Self::Decimal(_)
            | Self::String(_)
            | Self::Token(_)
            | Self::Boolean(_) => None,
        }
    }

    /// Strictly serialises the bare item (RFC 8941 §4.1.3).
    ///
    /// # Errors
    ///
    /// Returns [`StructuredFieldError`] when the value is outside its type's range or alphabet.
    pub fn serialize(&self) -> Result<String> {
        let mut out = String::new();
        self.write(&mut out)?;
        Ok(out)
    }

    fn write(&self, out: &mut String) -> Result<()> {
        match self {
            Self::Integer(value) => {
                if value.unsigned_abs() > MAX_INTEGER as u64 {
                    return Err(StructuredFieldError::new(format!(
                        "integer {value} is outside ±999,999,999,999,999"
                    )));
                }
                out.push_str(&value.to_string());
            }
            Self::Decimal(Decimal(thousandths)) => {
                let magnitude = thousandths.unsigned_abs();
                if magnitude / 1000 > MAX_DECIMAL_INTEGER_PART as u64 {
                    return Err(StructuredFieldError::new(
                        "decimal has more than 12 integer digits",
                    ));
                }
                if *thousandths < 0 {
                    out.push('-');
                }
                let fraction = format!("{:03}", magnitude % 1000);
                let fraction = fraction.trim_end_matches('0');
                let fraction = if fraction.is_empty() { "0" } else { fraction };
                out.push_str(&format!("{}.{fraction}", magnitude / 1000));
            }
            Self::String(value) => {
                out.push('"');
                for ch in value.chars() {
                    if !(' '..='~').contains(&ch) {
                        return Err(StructuredFieldError::new(format!(
                            "string contains a character outside printable ASCII: {ch:?}"
                        )));
                    }
                    if ch == '"' || ch == '\\' {
                        out.push('\\');
                    }
                    out.push(ch);
                }
                out.push('"');
            }
            Self::Token(value) => {
                if !is_token(value) {
                    return Err(StructuredFieldError::new(format!("`{value}` is not a token")));
                }
                out.push_str(value);
            }
            Self::ByteSequence(bytes) => {
                out.push(':');
                out.push_str(&STANDARD.encode(bytes));
                out.push(':');
            }
            Self::Boolean(value) => out.push_str(if *value { "?1" } else { "?0" }),
        }
        Ok(())
    }
}

/// Ordered RFC 8941 Parameters (§3.1.2).
///
/// # Example
///
/// ```rust
/// use adk_auth::web_bot_auth::structured::{BareItem, Parameters};
///
/// let mut params = Parameters::new();
/// params.insert("created", BareItem::Integer(1_618_884_473));
/// params.insert("keyid", BareItem::String("test-key-ed25519".into()));
/// assert_eq!(params.serialize().unwrap(), r#";created=1618884473;keyid="test-key-ed25519""#);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Parameters(Vec<(String, BareItem)>);

impl Parameters {
    /// Creates empty parameters.
    pub fn new() -> Self {
        Self::default()
    }

    /// The value of `key`.
    pub fn get(&self, key: &str) -> Option<&BareItem> {
        self.0.iter().find(|(name, _)| name == key).map(|(_, value)| value)
    }

    /// Sets `key`, replacing an existing value in place or appending a new one.
    pub fn insert(&mut self, key: impl Into<String>, value: BareItem) {
        let key = key.into();
        match self.0.iter_mut().find(|(name, _)| *name == key) {
            Some(entry) => entry.1 = value,
            None => self.0.push((key, value)),
        }
    }

    /// The parameters in order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &BareItem)> {
        self.0.iter().map(|(name, value)| (name.as_str(), value))
    }

    /// The number of parameters.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there are no parameters.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Strictly serialises the parameters (RFC 8941 §4.1.1.2).
    ///
    /// # Errors
    ///
    /// Returns [`StructuredFieldError`] for an invalid key or value.
    pub fn serialize(&self) -> Result<String> {
        let mut out = String::new();
        self.write(&mut out)?;
        Ok(out)
    }

    fn write(&self, out: &mut String) -> Result<()> {
        for (key, value) in &self.0 {
            out.push(';');
            write_key(key, out)?;
            if *value != BareItem::Boolean(true) {
                out.push('=');
                value.write(out)?;
            }
        }
        Ok(())
    }
}

/// An Item: a bare item with parameters (RFC 8941 §3.3).
///
/// # Example
///
/// ```rust
/// use adk_auth::web_bot_auth::structured::{BareItem, Item};
///
/// let item = Item::parse(r#""@query-param";name="baz""#).unwrap();
/// assert_eq!(item.bare_item, BareItem::String("@query-param".into()));
/// assert_eq!(item.params.get("name"), Some(&BareItem::String("baz".into())));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// The value.
    pub bare_item: BareItem,
    /// Its parameters.
    pub params: Parameters,
}

impl Item {
    /// Creates an item without parameters.
    pub fn new(bare_item: BareItem) -> Self {
        Self { bare_item, params: Parameters::new() }
    }

    /// Parses a field value as an Item (RFC 8941 §4.2).
    ///
    /// # Errors
    ///
    /// Returns [`StructuredFieldError`] when the value is not exactly one Item.
    pub fn parse(input: &str) -> Result<Self> {
        let mut parser = Parser::new(input)?;
        parser.skip_sp();
        let item = parser.parse_item()?;
        parser.finish()?;
        Ok(item)
    }

    /// Strictly serialises the item (RFC 8941 §4.1.3).
    ///
    /// # Errors
    ///
    /// Returns [`StructuredFieldError`] for an invalid value or parameter.
    pub fn serialize(&self) -> Result<String> {
        let mut out = String::new();
        self.write(&mut out)?;
        Ok(out)
    }

    fn write(&self, out: &mut String) -> Result<()> {
        self.bare_item.write(out)?;
        self.params.write(out)
    }
}

/// An Inner List: Items in parentheses, with parameters of its own (RFC 8941 §3.1.1).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InnerList {
    /// The members.
    pub items: Vec<Item>,
    /// The list's parameters.
    pub params: Parameters,
}

impl InnerList {
    /// Strictly serialises the inner list (RFC 8941 §4.1.1.1).
    ///
    /// # Errors
    ///
    /// Returns [`StructuredFieldError`] for an invalid member or parameter.
    pub fn serialize(&self) -> Result<String> {
        let mut out = String::new();
        self.write(&mut out)?;
        Ok(out)
    }

    fn write(&self, out: &mut String) -> Result<()> {
        out.push('(');
        for (index, item) in self.items.iter().enumerate() {
            if index > 0 {
                out.push(' ');
            }
            item.write(out)?;
        }
        out.push(')');
        self.params.write(out)
    }
}

/// A List or Dictionary member: an Item or an Inner List.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListEntry {
    /// An Item.
    Item(Item),
    /// An Inner List.
    InnerList(InnerList),
}

impl ListEntry {
    /// Strictly serialises the member value, as RFC 9421 §2.1.2 does for a Dictionary member.
    ///
    /// # Errors
    ///
    /// Returns [`StructuredFieldError`] for an invalid value or parameter.
    pub fn serialize(&self) -> Result<String> {
        let mut out = String::new();
        self.write(&mut out)?;
        Ok(out)
    }

    fn write(&self, out: &mut String) -> Result<()> {
        match self {
            Self::Item(item) => item.write(out),
            Self::InnerList(list) => list.write(out),
        }
    }
}

/// An RFC 8941 List (§3.1).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct List(pub Vec<ListEntry>);

impl List {
    /// Parses a field value as a List; an empty value is an empty List.
    ///
    /// # Errors
    ///
    /// Returns [`StructuredFieldError`] when the value is not a List.
    pub fn parse(input: &str) -> Result<Self> {
        let mut parser = Parser::new(input)?;
        parser.skip_sp();
        let mut members = Vec::new();
        while !parser.at_end() {
            members.push(parser.parse_item_or_inner_list()?);
            if parser.after_member()? {
                break;
            }
        }
        parser.finish()?;
        Ok(Self(members))
    }

    /// Strictly serialises the list (RFC 8941 §4.1.1).
    ///
    /// # Errors
    ///
    /// Returns [`StructuredFieldError`] for an invalid member.
    pub fn serialize(&self) -> Result<String> {
        let mut out = String::new();
        for (index, member) in self.0.iter().enumerate() {
            if index > 0 {
                out.push_str(", ");
            }
            member.write(&mut out)?;
        }
        Ok(out)
    }
}

/// An RFC 8941 Dictionary (§3.2): ordered, uniquely keyed members.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Dictionary(Vec<(String, ListEntry)>);

impl Dictionary {
    /// Creates an empty dictionary.
    pub fn new() -> Self {
        Self::default()
    }

    /// Parses a field value as a Dictionary; an empty value is an empty Dictionary. A repeated
    /// key keeps its first position and takes the last value (RFC 8941 §4.2.2).
    ///
    /// # Errors
    ///
    /// Returns [`StructuredFieldError`] when the value is not a Dictionary.
    pub fn parse(input: &str) -> Result<Self> {
        Self::parse_inner(input, false)
    }

    /// Parses like [`Dictionary::parse`] but rejects a repeated key, as RFC 9421 §4 requires
    /// of signature labels.
    ///
    /// # Errors
    ///
    /// Returns [`StructuredFieldError`] when the value is not a Dictionary or repeats a key.
    pub fn parse_unique(input: &str) -> Result<Self> {
        Self::parse_inner(input, true)
    }

    fn parse_inner(input: &str, unique: bool) -> Result<Self> {
        let mut parser = Parser::new(input)?;
        parser.skip_sp();
        let mut dict = Self::new();
        while !parser.at_end() {
            let key = parser.parse_key()?;
            let member = if parser.peek() == Some(b'=') {
                parser.pos += 1;
                parser.parse_item_or_inner_list()?
            } else {
                ListEntry::Item(Item {
                    bare_item: BareItem::Boolean(true),
                    params: parser.parse_parameters()?,
                })
            };
            if unique && dict.get(&key).is_some() {
                return Err(StructuredFieldError::new(format!("key `{key}` appears twice")));
            }
            dict.insert(key, member);
            if parser.after_member()? {
                break;
            }
        }
        parser.finish()?;
        Ok(dict)
    }

    /// The member under `key`.
    pub fn get(&self, key: &str) -> Option<&ListEntry> {
        self.0.iter().find(|(name, _)| name == key).map(|(_, value)| value)
    }

    /// Sets `key`, replacing an existing member in place or appending a new one.
    pub fn insert(&mut self, key: impl Into<String>, value: ListEntry) {
        let key = key.into();
        match self.0.iter_mut().find(|(name, _)| *name == key) {
            Some(entry) => entry.1 = value,
            None => self.0.push((key, value)),
        }
    }

    /// The members in order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &ListEntry)> {
        self.0.iter().map(|(name, value)| (name.as_str(), value))
    }

    /// The number of members.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there are no members.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Strictly serialises the dictionary (RFC 8941 §4.1.2).
    ///
    /// # Errors
    ///
    /// Returns [`StructuredFieldError`] for an invalid key or member.
    pub fn serialize(&self) -> Result<String> {
        let mut out = String::new();
        for (index, (key, member)) in self.0.iter().enumerate() {
            if index > 0 {
                out.push_str(", ");
            }
            write_key(key, &mut out)?;
            match member {
                ListEntry::Item(item) if item.bare_item == BareItem::Boolean(true) => {
                    item.params.write(&mut out)?;
                }
                ListEntry::Item(_) | ListEntry::InnerList(_) => {
                    out.push('=');
                    member.write(&mut out)?;
                }
            }
        }
        Ok(out)
    }
}

fn is_tchar(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

fn is_token(value: &str) -> bool {
    let bytes = value.as_bytes();
    match bytes.first() {
        Some(first) if first.is_ascii_alphabetic() || *first == b'*' => {
            bytes[1..].iter().all(|b| is_tchar(*b) || *b == b':' || *b == b'/')
        }
        _ => false,
    }
}

fn is_key_char(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-.*".contains(&byte)
}

fn write_key(key: &str, out: &mut String) -> Result<()> {
    let bytes = key.as_bytes();
    let valid = matches!(bytes.first(), Some(first) if first.is_ascii_lowercase() || *first == b'*')
        && bytes.iter().all(|b| is_key_char(*b));
    if !valid {
        return Err(StructuredFieldError::new(format!("`{key}` is not a valid key")));
    }
    out.push_str(key);
    Ok(())
}

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Result<Self> {
        if !input.is_ascii() {
            return Err(StructuredFieldError::new("field value is not ASCII"));
        }
        Ok(Self { input: input.as_bytes(), pos: 0 })
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    fn at_end(&self) -> bool {
        self.pos >= self.input.len()
    }

    fn skip_sp(&mut self) {
        while self.peek() == Some(b' ') {
            self.pos += 1;
        }
    }

    fn skip_ows(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.pos += 1;
        }
    }

    fn finish(&mut self) -> Result<()> {
        self.skip_sp();
        if self.at_end() { Ok(()) } else { Err(self.error("unexpected trailing characters")) }
    }

    /// Consumes the separator after a List or Dictionary member; `true` when the input ended.
    fn after_member(&mut self) -> Result<bool> {
        self.skip_ows();
        if self.at_end() {
            return Ok(true);
        }
        if self.peek() != Some(b',') {
            return Err(self.error("expected `,` between members"));
        }
        self.pos += 1;
        self.skip_ows();
        if self.at_end() {
            return Err(self.error("trailing `,`"));
        }
        Ok(false)
    }

    fn error(&self, reason: &str) -> StructuredFieldError {
        StructuredFieldError::new(format!("{reason} at byte {}", self.pos))
    }

    fn parse_item_or_inner_list(&mut self) -> Result<ListEntry> {
        if self.peek() == Some(b'(') {
            self.parse_inner_list().map(ListEntry::InnerList)
        } else {
            self.parse_item().map(ListEntry::Item)
        }
    }

    fn parse_inner_list(&mut self) -> Result<InnerList> {
        self.pos += 1;
        let mut items = Vec::new();
        loop {
            self.skip_sp();
            match self.peek() {
                None => return Err(self.error("unterminated inner list")),
                Some(b')') => {
                    self.pos += 1;
                    let params = self.parse_parameters()?;
                    return Ok(InnerList { items, params });
                }
                Some(_) => {
                    items.push(self.parse_item()?);
                    if !matches!(self.peek(), Some(b' ' | b')')) {
                        return Err(self.error("expected a space or `)` in inner list"));
                    }
                }
            }
        }
    }

    fn parse_item(&mut self) -> Result<Item> {
        let bare_item = self.parse_bare_item()?;
        let params = self.parse_parameters()?;
        Ok(Item { bare_item, params })
    }

    fn parse_parameters(&mut self) -> Result<Parameters> {
        let mut params = Parameters::new();
        while self.peek() == Some(b';') {
            self.pos += 1;
            self.skip_sp();
            let key = self.parse_key()?;
            let value = if self.peek() == Some(b'=') {
                self.pos += 1;
                self.parse_bare_item()?
            } else {
                BareItem::Boolean(true)
            };
            params.insert(key, value);
        }
        Ok(params)
    }

    fn parse_key(&mut self) -> Result<String> {
        match self.peek() {
            Some(first) if first.is_ascii_lowercase() || first == b'*' => {}
            _ => return Err(self.error("expected a key")),
        }
        let start = self.pos;
        while self.peek().is_some_and(is_key_char) {
            self.pos += 1;
        }
        Ok(self.slice(start))
    }

    fn slice(&self, start: usize) -> String {
        // The input is ASCII, so every byte range is valid UTF-8.
        String::from_utf8_lossy(&self.input[start..self.pos]).into_owned()
    }

    fn parse_bare_item(&mut self) -> Result<BareItem> {
        match self.peek() {
            Some(b'-' | b'0'..=b'9') => self.parse_number(),
            Some(b'"') => self.parse_string(),
            Some(b':') => self.parse_byte_sequence(),
            Some(b'?') => self.parse_boolean(),
            Some(byte) if byte.is_ascii_alphabetic() || byte == b'*' => {
                let start = self.pos;
                while self.peek().is_some_and(|b| is_tchar(b) || b == b':' || b == b'/') {
                    self.pos += 1;
                }
                Ok(BareItem::Token(self.slice(start)))
            }
            _ => Err(self.error("expected a bare item")),
        }
    }

    fn parse_number(&mut self) -> Result<BareItem> {
        let negative = self.peek() == Some(b'-');
        if negative {
            self.pos += 1;
        }
        if !self.peek().is_some_and(|b| b.is_ascii_digit()) {
            return Err(self.error("expected a digit"));
        }
        let start = self.pos;
        let mut decimal_point = None;
        while let Some(byte) = self.peek() {
            if byte.is_ascii_digit() {
                self.pos += 1;
            } else if byte == b'.' && decimal_point.is_none() {
                if self.pos - start > 12 {
                    return Err(self.error("decimal has more than 12 integer digits"));
                }
                decimal_point = Some(self.pos);
                self.pos += 1;
            } else {
                break;
            }
            let length = self.pos - start;
            if (decimal_point.is_none() && length > 15) || (decimal_point.is_some() && length > 16)
            {
                return Err(self.error("number is too long"));
            }
        }
        let text = self.slice(start);
        let sign = if negative { -1 } else { 1 };
        match decimal_point {
            None => {
                let value: i64 = text.parse().map_err(|_| self.error("invalid integer"))?;
                Ok(BareItem::Integer(sign * value))
            }
            Some(point) => {
                let fraction_digits = self.pos - point - 1;
                if fraction_digits == 0 || fraction_digits > 3 {
                    return Err(self.error("decimal needs one to three fractional digits"));
                }
                let (integer, fraction) = text.split_once('.').unwrap_or((&text, "0"));
                let integer: i64 = integer.parse().map_err(|_| self.error("invalid decimal"))?;
                let fraction: i64 =
                    format!("{fraction:0<3}").parse().map_err(|_| self.error("invalid decimal"))?;
                Ok(BareItem::Decimal(Decimal(sign * (integer * 1000 + fraction))))
            }
        }
    }

    fn parse_string(&mut self) -> Result<BareItem> {
        self.pos += 1;
        let mut value = String::new();
        loop {
            let Some(byte) = self.peek() else {
                return Err(self.error("unterminated string"));
            };
            self.pos += 1;
            match byte {
                b'\\' => match self.peek() {
                    Some(escaped @ (b'"' | b'\\')) => {
                        self.pos += 1;
                        value.push(char::from(escaped));
                    }
                    _ => return Err(self.error("invalid escape in string")),
                },
                b'"' => return Ok(BareItem::String(value)),
                0x20..=0x7e => value.push(char::from(byte)),
                _ => return Err(self.error("string contains a control character")),
            }
        }
    }

    fn parse_byte_sequence(&mut self) -> Result<BareItem> {
        self.pos += 1;
        let start = self.pos;
        while let Some(byte) = self.peek() {
            if byte == b':' {
                let content = &self.input[start..self.pos];
                self.pos += 1;
                if !content.iter().all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(b)) {
                    return Err(self.error("byte sequence is not base64"));
                }
                let bytes = LENIENT_BASE64
                    .decode(content)
                    .map_err(|e| self.error(&format!("byte sequence is not base64: {e}")))?;
                return Ok(BareItem::ByteSequence(bytes));
            }
            self.pos += 1;
        }
        Err(self.error("unterminated byte sequence"))
    }

    fn parse_boolean(&mut self) -> Result<BareItem> {
        self.pos += 1;
        let value = match self.peek() {
            Some(b'1') => true,
            Some(b'0') => false,
            _ => return Err(self.error("boolean must be ?0 or ?1")),
        };
        self.pos += 1;
        Ok(BareItem::Boolean(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc_9421_dictionary_member_examples_reserialize() {
        // RFC 9421 §2.1.2: `Example-Dict:  a=1, b=2;x=1;y=2, c=(a   b    c), d`.
        let dict = Dictionary::parse("a=1, b=2;x=1;y=2, c=(a   b    c), d").unwrap();
        let member = |key: &str| dict.get(key).unwrap().serialize().unwrap();
        assert_eq!(member("a"), "1");
        assert_eq!(member("d"), "?1");
        assert_eq!(member("b"), "2;x=1;y=2");
        assert_eq!(member("c"), "(a b c)");
        assert_eq!(dict.serialize().unwrap(), "a=1, b=2;x=1;y=2, c=(a b c), d");
    }

    #[test]
    fn rfc_9421_strict_serialization_example() {
        // RFC 9421 §2.1.1: `"example-dict";sf` reserialises optional whitespace.
        let dict = Dictionary::parse("a=1,    b=2;x=1;y=2,   c=(a   b   c)").unwrap();
        assert_eq!(dict.serialize().unwrap(), "a=1, b=2;x=1;y=2, c=(a b c)");
    }

    #[test]
    fn decimals_round_trip_exactly() {
        let list = List::parse("1.5, -0.25, 3.0, 999999999999.999").unwrap();
        assert_eq!(list.serialize().unwrap(), "1.5, -0.25, 3.0, 999999999999.999");
        assert!(List::parse("1.2345").is_err());
        assert!(List::parse("1.").is_err());
        assert!(List::parse("1234567890123.0").is_err());
    }

    #[test]
    fn integers_are_bounded() {
        assert!(Item::parse("999999999999999").is_ok());
        assert!(Item::parse("1999999999999999").is_err());
        assert!(BareItem::Integer(MAX_INTEGER + 1).serialize().is_err());
    }

    #[test]
    fn strings_escape_quotes_and_backslashes() {
        let item = Item::parse(r#""say \"hi\" \\ bye""#).unwrap();
        assert_eq!(item.bare_item, BareItem::String(r#"say "hi" \ bye"#.into()));
        assert_eq!(item.serialize().unwrap(), r#""say \"hi\" \\ bye""#);
        assert!(Item::parse(r#""bad \n escape""#).is_err());
        assert!(BareItem::String("tab\there".into()).serialize().is_err());
    }

    #[test]
    fn byte_sequences_accept_missing_padding() {
        let item = Item::parse(":aGVsbG8:").unwrap();
        assert_eq!(item.bare_item, BareItem::ByteSequence(b"hello".to_vec()));
        assert_eq!(item.serialize().unwrap(), ":aGVsbG8=:");
        assert!(Item::parse(":not*base64:").is_err());
    }

    #[test]
    fn tokens_and_booleans() {
        let list = List::parse("*foo/bar:baz, ?0, ?1;a").unwrap();
        assert_eq!(list.serialize().unwrap(), "*foo/bar:baz, ?0, ?1;a");
        assert!(Item::parse("?2").is_err());
    }

    #[test]
    fn malformed_values_fail() {
        for input in ["a=1,", "a=(1 2", "A=1", "a=1 b=2", "(1)(2)", "a=\"x", "a=1,,b=2"] {
            assert!(Dictionary::parse(input).is_err(), "{input} should fail");
        }
        assert!(Dictionary::parse("").unwrap().is_empty());
        assert!(Item::parse("").is_err());
        assert!(Item::parse("é").is_err());
    }

    #[test]
    fn duplicate_keys_overwrite_or_fail_when_unique() {
        let dict = Dictionary::parse("a=1, b=2, a=3").unwrap();
        assert_eq!(dict.serialize().unwrap(), "a=3, b=2");
        assert!(Dictionary::parse_unique("a=1, b=2, a=3").is_err());
    }

    #[test]
    fn invalid_keys_do_not_serialize() {
        let mut dict = Dictionary::new();
        dict.insert("Upper", ListEntry::Item(Item::new(BareItem::Integer(1))));
        assert!(dict.serialize().is_err());
    }
}
