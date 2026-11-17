//! Claude's client toolsets on ADK's function-call surface.
//!
//! A member call — `{"name": "left_click", "toolset_name": "computer"}` from the toolset,
//! or `{"name": "computer", "input": {"action": "left_click"}}` from an earlier computer
//! use version — becomes the function call `computer.left_click` with the member's input
//! as its arguments, so one set of tools serves every version. Requests convert back to
//! the form the request declares, and each member result echoes its `toolset_name`.
//!
//! ADK tools declare membership with [`MEMBER_KEY`]; the declarations of one toolset
//! collapse into a single `tools` entry whose `configs` offer exactly the members
//! declared, at the version the request's model accepts.

use super::error::ConversionError;
use crate::attachment;
use adk_anthropic::{
    BROWSER_TOOLSET, Base64ImageSource, BrowserMember, BrowserStateBlock, BrowserToolset20260801,
    COMPUTER_TOOLSET, ComputerMember, ComputerToolVersion, ComputerToolset20260801, ImageBlock,
    ImageMediaType, TextBlock, ToolComputer20250124, ToolComputer20251124, ToolResultBlockContent,
    ToolUnionParam, ToolUseBlock, UrlImageSource,
};
use adk_core::FunctionResponseData;
use serde_json::Value;

/// Declaration key naming the client toolset member an ADK tool implements:
/// `{"toolset": "computer" | "browser", "member": "<member>"}`, plus
/// `display_width_px` and `display_height_px` on a computer member, which a model that
/// takes an earlier computer use version needs.
pub(super) const MEMBER_KEY: &str = "x-adk-anthropic-toolset-member";

/// Which toolset a member belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Toolset {
    Computer(ComputerMember),
    Browser(BrowserMember),
}

/// The function call name for a member: `computer.left_click`, `browser.navigate`.
fn call_name(toolset: &str, member: &str) -> String {
    format!("{toolset}.{member}")
}

/// The member a function call name addresses, if it addresses one.
pub(super) fn member_of_call(name: &str) -> Option<Toolset> {
    let (toolset, member) = name.split_once('.')?;
    match toolset {
        COMPUTER_TOOLSET => ComputerMember::from_name(member).map(Toolset::Computer),
        BROWSER_TOOLSET => BrowserMember::from_name(member).map(Toolset::Browser),
        _ => None,
    }
}

/// How the request declares each toolset, which decides the shape of member calls and
/// results in its history and of the calls in its response.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct ToolsetForms {
    /// The computer use version the request declares.
    pub computer: Option<ComputerToolVersion>,
}

impl ToolsetForms {
    /// The forms the final `tools` of a request declare.
    pub(super) fn of(tools: &[ToolUnionParam]) -> Self {
        Self { computer: tools.iter().find_map(ToolUnionParam::computer_tool_version) }
    }

    /// Whether computer calls take the earlier one-tool form.
    fn legacy_computer(self) -> bool {
        self.computer.is_some_and(|version| !version.is_toolset())
    }
}

/// The function call a model `tool_use` block becomes.
pub(super) fn call_from_tool_use(block: &ToolUseBlock, forms: ToolsetForms) -> (String, Value) {
    if let Some(toolset) = &block.toolset_name {
        return (call_name(toolset, &block.name), block.input.clone());
    }
    if forms.legacy_computer()
        && block.name == COMPUTER_TOOLSET
        && let Value::Object(input) = &block.input
        && let Some(action) = input.get("action").and_then(Value::as_str)
        && ComputerMember::from_name(action).is_some()
    {
        let mut args = input.clone();
        args.remove("action");
        return (call_name(COMPUTER_TOOLSET, action), Value::Object(args));
    }
    (block.name.clone(), block.input.clone())
}

/// The `tool_use` block a function call in the history becomes.
pub(super) fn tool_use_from_call(
    id: String,
    name: &str,
    args: &Value,
    forms: ToolsetForms,
) -> ToolUseBlock {
    match member_of_call(name) {
        Some(Toolset::Computer(member)) if forms.legacy_computer() => {
            let mut input = match args {
                Value::Object(map) => map.clone(),
                _ => serde_json::Map::new(),
            };
            input.insert("action".to_string(), Value::String(member.as_str().to_string()));
            ToolUseBlock::new(id, COMPUTER_TOOLSET, Value::Object(input))
        }
        Some(Toolset::Computer(member)) => {
            ToolUseBlock::member(id, COMPUTER_TOOLSET, member.as_str(), args.clone())
        }
        Some(Toolset::Browser(member)) => {
            ToolUseBlock::member(id, BROWSER_TOOLSET, member.as_str(), args.clone())
        }
        None => ToolUseBlock::new(id, name, args.clone()),
    }
}

/// The `toolset_name` a result for the function call `name` echoes, if any.
pub(super) fn result_toolset(name: &str, forms: ToolsetForms) -> Option<&'static str> {
    match member_of_call(name)? {
        Toolset::Computer(_) if forms.legacy_computer() => None,
        Toolset::Computer(_) => Some(COMPUTER_TOOLSET),
        Toolset::Browser(_) => Some(BROWSER_TOOLSET),
    }
}

fn image_media_type(mime_type: &str) -> Option<ImageMediaType> {
    match mime_type {
        "image/jpeg" => Some(ImageMediaType::Jpeg),
        "image/png" => Some(ImageMediaType::Png),
        "image/gif" => Some(ImageMediaType::Gif),
        "image/webp" => Some(ImageMediaType::Webp),
        _ => None,
    }
}

/// The content of a toolset member result.
///
/// A member result may hold only text, images and, for the browser toolset, one
/// `browser_state` block. A tool answers with a string, or with an object whose `text` is
/// the acknowledgment and whose `browser_state` is the tab state; images come from the
/// response's inline data. An error result never carries tab state.
pub(super) fn member_result_content(
    response: &FunctionResponseData,
    toolset: &str,
    is_error: bool,
) -> ToolResultBlockContent {
    let (text, state) = match &response.response {
        Value::String(text) => (text.clone(), None),
        Value::Object(map) if map.contains_key("text") || map.contains_key("browser_state") => {
            let text = map.get("text").and_then(Value::as_str).unwrap_or_default().to_string();
            let state = map
                .get("browser_state")
                .filter(|_| toolset == BROWSER_TOOLSET && !is_error)
                .and_then(|state| match serde_json::from_value::<BrowserStateBlock>(state.clone()) {
                    Ok(state) => Some(state),
                    Err(error) => {
                        tracing::warn!(error = %error, "browser_state in a tool result is malformed; leaving it out");
                        None
                    }
                });
            (text, state)
        }
        Value::Null => (String::new(), None),
        other => (other.to_string(), None),
    };
    let mut items = Vec::new();
    if !text.trim().is_empty() {
        items.push(adk_anthropic::Content::Text(TextBlock::new(text.clone())));
    }
    for data in &response.inline_data {
        items.push(match image_media_type(&data.mime_type) {
            Some(media_type) => adk_anthropic::Content::Image(ImageBlock::new_with_base64(
                Base64ImageSource::new(attachment::encode_base64(&data.data), media_type),
            )),
            None => adk_anthropic::Content::Text(TextBlock::new(
                attachment::inline_attachment_to_text(&data.mime_type, &data.data),
            )),
        });
    }
    for file in &response.file_data {
        if image_media_type(&file.mime_type).is_some() {
            items.push(adk_anthropic::Content::Image(ImageBlock::new_with_url(
                UrlImageSource::new(file.file_uri.clone()),
            )));
        }
    }
    if let Some(state) = state {
        items.push(adk_anthropic::Content::BrowserState(state));
    }
    match items.as_slice() {
        [] => ToolResultBlockContent::String(if is_error { "Error" } else { "OK" }.to_string()),
        [adk_anthropic::Content::Text(_)] => ToolResultBlockContent::String(text),
        _ => ToolResultBlockContent::Array(items),
    }
}

/// Members collected from ADK tool declarations, in the order they were declared.
#[derive(Default)]
pub(super) struct DeclaredMembers {
    computer: Vec<ComputerMember>,
    display: Option<(u32, u32)>,
    browser: Vec<BrowserMember>,
}

impl DeclaredMembers {
    /// Whether a computer use member has been recorded.
    pub(super) fn has_computer(&self) -> bool {
        !self.computer.is_empty()
    }

    /// Whether a browser use member has been recorded.
    pub(super) fn has_browser(&self) -> bool {
        !self.browser.is_empty()
    }

    /// Records the member a declaration carries. Returns `Ok(false)` when the declaration
    /// carries no member marker.
    pub(super) fn record(
        &mut self,
        name: &str,
        declaration: &Value,
    ) -> Result<bool, ConversionError> {
        let Some(marker) = declaration.get(MEMBER_KEY) else {
            return Ok(false);
        };
        let toolset = marker.get("toolset").and_then(Value::as_str).unwrap_or_default();
        let member = marker.get("member").and_then(Value::as_str).unwrap_or_default();
        let invalid = || {
            ConversionError::InvalidToolDeclaration(format!(
                "tool '{name}' declares toolset member '{toolset}.{member}', which \
                 computer_toolset_20260801 and browser_toolset_20260801 do not define"
            ))
        };
        match toolset {
            COMPUTER_TOOLSET => {
                let member = ComputerMember::from_name(member).ok_or_else(invalid)?;
                if !self.computer.contains(&member) {
                    self.computer.push(member);
                }
                let width = marker.get("display_width_px").and_then(Value::as_u64);
                let height = marker.get("display_height_px").and_then(Value::as_u64);
                if let (Some(width), Some(height)) = (width, height) {
                    self.display = Some((width as u32, height as u32));
                }
            }
            BROWSER_TOOLSET => {
                let member = BrowserMember::from_name(member).ok_or_else(invalid)?;
                if !self.browser.contains(&member) {
                    self.browser.push(member);
                }
            }
            _ => return Err(invalid()),
        }
        Ok(true)
    }

    /// The computer use entry the declared members make for `model`, if any were declared:
    /// the toolset offering exactly those members, or the earlier version the model takes.
    pub(super) fn computer_entry(
        &self,
        model: &str,
    ) -> Result<Option<ToolUnionParam>, ConversionError> {
        if self.computer.is_empty() {
            return Ok(None);
        }
        let toolset = ComputerMember::ALL
            .iter()
            .filter(|member| !self.computer.contains(member))
            .fold(ComputerToolset20260801::new(), |entry, member| {
                entry.with_member(*member, false)
            });
        let zoom = self.computer.contains(&ComputerMember::Zoom);
        let target =
            ComputerToolVersion::for_model(model).unwrap_or(ComputerToolVersion::Toolset20260801);
        let entry = match (target, self.display) {
            (ComputerToolVersion::Toolset20260801, _) => {
                ToolUnionParam::ComputerToolset20260801(toolset)
            }
            (ComputerToolVersion::Computer20251124, Some((width, height))) => {
                let entry = ToolComputer20251124::new(width, height);
                ToolUnionParam::Computer20251124(if zoom { entry.with_zoom(true) } else { entry })
            }
            (_, Some((width, height))) => {
                ToolUnionParam::Computer20250124(ToolComputer20250124::new(width, height))
            }
            (earlier, None) => return Err(needs_display(model, earlier)),
        };
        Ok(Some(entry))
    }

    /// The browser toolset entry the declared members make, if any were declared.
    pub(super) fn browser_entry(&self) -> Option<ToolUnionParam> {
        if self.browser.is_empty() {
            return None;
        }
        let entry =
            BrowserMember::ALL.iter().fold(BrowserToolset20260801::new(), |entry, member| {
                let declared = self.browser.contains(member);
                if declared == member.enabled_by_default() {
                    entry
                } else {
                    entry.with_member(*member, declared)
                }
            });
        Some(ToolUnionParam::BrowserToolset20260801(entry))
    }
}

/// Puts every computer and browser use entry in the form `model` accepts.
///
/// A computer entry takes the version [`ComputerToolVersion::for_model`] names; an earlier
/// version needs the display size, so an entry without one fails with guidance. A model
/// that names no Claude model keeps the declared form. A browser entry fails for a model
/// that does not accept the browser toolset. Each toolset may be declared once.
pub(super) fn resolve_for_model(
    model: &str,
    tools: &mut [ToolUnionParam],
) -> Result<(), ConversionError> {
    let mut computer_entries = 0;
    let mut browser_entries = 0;
    for tool in tools.iter_mut() {
        if let Some(declared) = tool.computer_tool_version() {
            computer_entries += 1;
            let target = ComputerToolVersion::for_model(model).unwrap_or(declared);
            *tool = computer_at_version(model, tool, target)?;
        }
        if matches!(tool, ToolUnionParam::BrowserToolset20260801(_)) {
            browser_entries += 1;
            if !adk_anthropic::supports_browser_toolset(model) {
                return Err(ConversionError::InvalidToolDeclaration(format!(
                    "model '{model}' does not accept browser_toolset_20260801; use a model \
                     that does (claude-sonnet-5-5, claude-haiku-5-5, claude-opus-5-5, \
                     claude-fable-5-2, claude-opus-4-8), or give the agent adk-browser's \
                     BrowserToolset function tools instead"
                )));
            }
        }
    }
    if computer_entries > 1 || browser_entries > 1 {
        return Err(ConversionError::InvalidToolDeclaration(
            "computer use and browser use may each be declared once per request; remove the \
             duplicate toolset declaration"
                .to_string(),
        ));
    }
    Ok(())
}

fn needs_display(model: &str, version: ComputerToolVersion) -> ConversionError {
    ConversionError::InvalidToolDeclaration(format!(
        "model '{model}' takes computer use as {}, which needs the display size; declare \
         display_width_px and display_height_px on the computer use tools, or use a model \
         that takes computer_toolset_20260801",
        version.type_name()
    ))
}

fn computer_at_version(
    model: &str,
    tool: &ToolUnionParam,
    target: ComputerToolVersion,
) -> Result<ToolUnionParam, ConversionError> {
    let (display, zoom, cache_control, configs) = match tool {
        ToolUnionParam::ComputerToolset20260801(entry) => (
            None,
            entry.is_enabled(ComputerMember::Zoom),
            entry.cache_control.clone(),
            Some(entry.clone()),
        ),
        ToolUnionParam::Computer20251124(entry) => (
            Some((entry.display_width_px, entry.display_height_px, entry.display_number)),
            entry.enable_zoom == Some(true),
            entry.cache_control.clone(),
            None,
        ),
        ToolUnionParam::Computer20250124(entry) => (
            Some((entry.display_width_px, entry.display_height_px, entry.display_number)),
            false,
            entry.cache_control.clone(),
            None,
        ),
        other => return Ok(other.clone()),
    };
    Ok(match target {
        ComputerToolVersion::Toolset20260801 => {
            let mut entry = configs.unwrap_or_else(|| {
                ComputerToolset20260801::new().with_member(ComputerMember::Zoom, zoom)
            });
            if entry.configs.get("zoom").is_some_and(|config| config.enabled == Some(true)) {
                entry.configs.remove("zoom");
            }
            entry.cache_control = cache_control;
            ToolUnionParam::ComputerToolset20260801(entry)
        }
        earlier => {
            let Some((width, height, display_number)) = display else {
                return Err(needs_display(model, earlier));
            };
            if earlier == ComputerToolVersion::Computer20251124 {
                let mut entry = ToolComputer20251124::new(width, height).with_zoom(zoom);
                entry.display_number = display_number;
                entry.cache_control = cache_control;
                if !zoom {
                    entry.enable_zoom = None;
                }
                ToolUnionParam::Computer20251124(entry)
            } else {
                let mut entry = ToolComputer20250124::new(width, height);
                entry.display_number = display_number;
                entry.cache_control = cache_control;
                ToolUnionParam::Computer20250124(entry)
            }
        }
    })
}

#[cfg(test)]
mod tests;
