//! `[[kernel.webhooks]]`: the signed webhooks `adk-rust up` serves at `POST /hooks/{name}`,
//! whose deliveries wake the kernel's sleepers and triggers (spec 3.5 §8.2, §10 X11; spec
//! 3.2 §7.4).
//!
//! ```toml
//! [[kernel.webhooks]]
//! name = "github"
//! secret = "env:GITHUB_WEBHOOK_SECRET"   # or "file:~/.config/adk-rust/github.whsec"
//! ```
//!
//! Each delivery is verified with Standard Webhooks signatures and becomes a `kernel.wake`
//! run keyed `hook:<name>:<webhook-id>`, so a redelivery wakes nothing twice; a process
//! made with `adk-rust wake --webhook <name>` wakes on it. The secret is never written in
//! the file itself.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use adk_core::DurableRunStore;
use adk_core::webhook::{WebhookSecret, WebhookVerifier};
use adk_server::webhooks::WebhookIngress;
use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// One `[[kernel.webhooks]]` entry as written.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HookTable {
    name: String,
    secret: String,
}

/// The ingress `up` mounts, and the hooks it serves.
pub(super) struct Hooks {
    pub(super) ingress: WebhookIngress,
    pub(super) names: Vec<String>,
}

/// The webhook ingress over `runs` for the hooks the user config at `path` lists, or `None`
/// when it lists none.
pub(super) fn ingress(
    path: Option<&Path>,
    runs: Arc<dyn DurableRunStore>,
    app_name: &str,
) -> Result<Option<Hooks>> {
    let Some(path) = path else { return Ok(None) };
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("cannot read {}", path.display())),
    };
    let file: toml::Table =
        toml::from_str(&text).with_context(|| format!("{} is not valid TOML", path.display()))?;
    let Some(hooks) = file.get("kernel").and_then(|kernel| kernel.get("webhooks")) else {
        return Ok(None);
    };
    let tables: Vec<HookTable> = hooks.clone().try_into().with_context(|| {
        format!(
            "{}: `kernel.webhooks` is a list of tables with `name` and `secret`",
            path.display()
        )
    })?;
    let base = path.parent().unwrap_or(Path::new("."));
    let mut ingress = WebhookIngress::new(runs);
    let mut names = BTreeSet::new();
    for table in tables {
        let valid = !table.name.is_empty()
            && table.name.len() <= 64
            && table.name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if !valid {
            bail!(
                "{}: webhook `{}` needs a name of letters, digits, `-` and `_`; it is the path \
                 /hooks/<name>",
                path.display(),
                table.name
            );
        }
        if !names.insert(table.name.clone()) {
            bail!("{}: webhook `{}` is listed twice", path.display(), table.name);
        }
        let secret = secret(&table.secret, base)
            .with_context(|| format!("{}: webhook `{}`", path.display(), table.name))?;
        ingress = ingress.with_hook(adk_kernel::webhook_wake_hook(
            table.name,
            WebhookVerifier::new(secret),
            app_name,
        ));
    }
    if names.is_empty() {
        return Ok(None);
    }
    Ok(Some(Hooks { ingress, names: names.into_iter().collect() }))
}

/// A Standard Webhooks secret from `env:VAR` or `file:PATH`; a relative path is relative to
/// the user config's directory and `~/` is the home directory.
fn secret(source: &str, base: &Path) -> Result<WebhookSecret> {
    let text = if let Some(var) = source.strip_prefix("env:") {
        std::env::var(var).with_context(|| format!("the environment holds no `{var}`"))?
    } else if let Some(file) = source.strip_prefix("file:") {
        let file = match file.strip_prefix("~/") {
            Some(rest) => dirs::home_dir().context("no home directory for `~/`")?.join(rest),
            None => base.join(PathBuf::from(file)),
        };
        std::fs::read_to_string(&file).with_context(|| format!("cannot read {}", file.display()))?
    } else {
        bail!(
            "write the secret as `env:VAR` or `file:PATH`, never in the file itself; it is a \
             Standard Webhooks `whsec_` secret"
        );
    };
    WebhookSecret::from_base64(text.trim())
        .map_err(|error| anyhow::anyhow!("the secret is not a whsec_ base64 secret: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, text: &str) -> PathBuf {
        let path = dir.join("harness.toml");
        std::fs::write(&path, text).unwrap();
        path
    }

    fn runs() -> Arc<dyn DurableRunStore> {
        Arc::new(adk_core::run_store::InMemoryRunStore::new())
    }

    #[test]
    fn hooks_come_from_the_user_config_with_secrets_from_files() {
        let dir = tempfile::tempdir().unwrap();
        let secret = WebhookSecret::from_bytes(b"a shared signing key".to_vec()).unwrap();
        std::fs::write(dir.path().join("github.whsec"), format!("{}\n", secret.to_whsec()))
            .unwrap();
        let path = write(
            dir.path(),
            "[[kernel.webhooks]]\nname = \"github\"\nsecret = \"file:github.whsec\"\n",
        );
        let hooks = ingress(Some(&path), runs(), "adk-rust").unwrap().unwrap();
        assert_eq!(hooks.names, ["github"]);
        assert!(ingress(None, runs(), "adk-rust").unwrap().is_none());
        let empty = write(dir.path(), "[kernel]\nbind = \"127.0.0.1:8080\"\n");
        assert!(ingress(Some(&empty), runs(), "adk-rust").unwrap().is_none());
    }

    #[test]
    fn inline_secrets_bad_names_and_duplicates_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        for text in [
            "[[kernel.webhooks]]\nname = \"github\"\nsecret = \"whsec_c2VjcmV0\"\n",
            "[[kernel.webhooks]]\nname = \"../x\"\nsecret = \"env:ADK_TEST_NO_SUCH_VAR\"\n",
            "[[kernel.webhooks]]\nname = \"a\"\nsecret = \"env:ADK_TEST_NO_SUCH_VAR\"\n",
            "[[kernel.webhooks]]\nname = \"a\"\nsecret = \"file:missing\"\nextra = 1\n",
        ] {
            let path = write(dir.path(), text);
            assert!(ingress(Some(&path), runs(), "adk-rust").is_err(), "{text}");
        }
        std::fs::write(dir.path().join("s"), "whsec_c2VjcmV0").unwrap();
        let path = write(
            dir.path(),
            "[[kernel.webhooks]]\nname = \"a\"\nsecret = \"file:s\"\n\n[[kernel.webhooks]]\nname = \"a\"\nsecret = \"file:s\"\n",
        );
        assert!(ingress(Some(&path), runs(), "adk-rust").is_err());
    }
}
