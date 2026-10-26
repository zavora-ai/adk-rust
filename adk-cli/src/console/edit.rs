//! Editing a call's JSON arguments at the console (spec 3.1 §8.1).

use serde_json::Value;

use super::{ConsoleInput, ConsoleIo};

/// What an edit of a call's arguments produced.
pub(crate) enum Edited {
    Args(Value),
    Unchanged(String),
    Interrupted,
}

/// Lets the person replace `args`: in `$VISUAL`/`$EDITOR` on a real terminal, otherwise as
/// one typed line of JSON.
pub(crate) async fn edit_json(console: &ConsoleIo, args: &Value) -> Edited {
    let current = serde_json::to_string_pretty(args).unwrap_or_else(|_| args.to_string());
    let text = match console.can_open_editor().then(external_editor).flatten() {
        Some(editor) => match run_editor(editor, current, "json").await {
            Ok(text) => text,
            Err(error) => return Edited::Unchanged(format!("editor failed: {error}")),
        },
        None => match console.read_line("  arguments (JSON) > ").await {
            ConsoleInput::Line(text) => text,
            ConsoleInput::Interrupted => return Edited::Interrupted,
            ConsoleInput::Eof => return Edited::Unchanged("arguments unchanged".into()),
        },
    };
    if text.trim().is_empty() {
        return Edited::Unchanged("arguments unchanged".into());
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(edited) => Edited::Args(edited),
        Err(error) => Edited::Unchanged(format!("not valid JSON ({error}); arguments unchanged")),
    }
}

/// Lets the person edit the document `current`: in `$VISUAL`/`$EDITOR` on a real terminal,
/// otherwise as one typed line. `Ok(None)` when nothing changed or the editor failed (the
/// failure is printed); `Err(())` when the person pressed Ctrl-C.
pub(crate) async fn edit_text(
    console: &ConsoleIo,
    current: &str,
    extension: &str,
) -> Result<Option<String>, ()> {
    let text = match console.can_open_editor().then(external_editor).flatten() {
        Some(editor) => match run_editor(editor, current.to_string(), extension).await {
            Ok(text) => text,
            Err(error) => {
                console.print(&format!("  editor failed: {error}\n"));
                return Ok(None);
            }
        },
        None => match console.read_line("  calls (JSON array) > ").await {
            ConsoleInput::Line(text) => text,
            ConsoleInput::Interrupted => return Err(()),
            ConsoleInput::Eof => return Ok(None),
        },
    };
    Ok((!text.trim().is_empty() && text != current).then_some(text))
}

/// The editor named by `$VISUAL` or `$EDITOR`.
fn external_editor() -> Option<String> {
    ["VISUAL", "EDITOR"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|editor| !editor.trim().is_empty())
}

/// Opens `current` in `editor` as a `.{extension}` file and returns the saved text.
async fn run_editor(editor: String, current: String, extension: &str) -> std::io::Result<String> {
    let extension = extension.to_string();
    tokio::task::spawn_blocking(move || {
        let path = std::env::temp_dir().join(format!(
            "adk-tool-args-{}-{}.{extension}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos())
        ));
        std::fs::write(&path, current)?;
        let mut words = editor.split_whitespace();
        let program = words.next().unwrap_or("vi");
        let status = std::process::Command::new(program).args(words).arg(&path).status();
        let text = std::fs::read_to_string(&path);
        let _ = std::fs::remove_file(&path);
        match status? {
            exit if exit.success() => text,
            exit => Err(std::io::Error::other(format!("{program} exited with {exit}"))),
        }
    })
    .await
    .map_err(std::io::Error::other)?
}
