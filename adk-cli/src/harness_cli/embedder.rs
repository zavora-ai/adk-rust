//! What embeds long-term facts (spec 3.1 §7.15.2, §7.32, §13 Q9).
//!
//! Every user message is part of a recall query, so the embedder is a data boundary: the
//! facts and the queries go only to the chat provider's own embedder or to a model on this
//! machine, unless the user chose another with `--embedder` or `[memory] embedder`.

use std::sync::Arc;

use adk_cli::harness::EmbedderArg;
use adk_model::ModelProvider;
use anyhow::{Result, bail};

use crate::ResolvedModel;
use crate::cli::Provider;

/// The local ONNX model, as the banner names it.
const LOCAL: &str = "a local ONNX model (BAAI/bge-small-en-v1.5); nothing leaves this machine";
/// Lexical recall, as the banner names it.
pub(super) const LEXICAL: &str =
    "none: facts are found by shared words; nothing leaves this machine";

/// The embedder for a chat model of `provider` and the user's `explicit` choice
/// (spec 3.1 §7.32): the explicit choice; else the provider's own embedder for Gemini and
/// OpenAI, whose servers already see the conversation; else the local ONNX model; and
/// lexical recall for the scripted test model.
pub(super) fn choose(provider: Option<Provider>, explicit: Option<EmbedderArg>) -> EmbedderArg {
    if let Some(choice) = explicit {
        return choice;
    }
    match provider {
        None => EmbedderArg::Lexical,
        // Ollama and mistral.rs: nothing leaves this machine but the model endpoint (H10).
        Some(provider) if provider.is_local() => EmbedderArg::Local,
        Some(Provider::Model(ModelProvider::Gemini)) => EmbedderArg::Gemini,
        Some(Provider::Model(ModelProvider::Openai)) => EmbedderArg::Openai,
        Some(
            Provider::Model(
                ModelProvider::Anthropic
                | ModelProvider::Deepseek
                | ModelProvider::Groq
                | ModelProvider::Ollama,
            )
            | Provider::MistralRs,
        ) => EmbedderArg::Local,
    }
}

/// The embedder [`choose`] picks for `model`, built, with the name the banner shows; `None`
/// is lexical recall. A provider the user did not choose is never a fallback: without its
/// key the local model is used, and without the local model, lexical recall.
///
/// # Errors
///
/// Returns an error when the user chose an embedder that cannot be built: a provider
/// without its key, or `local` in a build without feature `memory-local`.
pub(super) fn build(
    model: &ResolvedModel,
    explicit: Option<EmbedderArg>,
) -> Result<Option<(Arc<adk_harness::RagEmbedder>, String)>> {
    let choice = choose(model.provider(), explicit);
    let (own, env): (ModelProvider, &[&str]) = match choice {
        EmbedderArg::Lexical => return Ok(None),
        EmbedderArg::Local => return local(explicit.is_some()),
        EmbedderArg::Gemini => (ModelProvider::Gemini, &["GOOGLE_API_KEY", "GEMINI_API_KEY"]),
        EmbedderArg::Openai => (ModelProvider::Openai, &["OPENAI_API_KEY"]),
    };
    let chat_key = model
        .api_key
        .clone()
        .filter(|_| model.provider() == Some(Provider::Model(own)))
        .filter(|key| !key.trim().is_empty());
    let key = chat_key.or_else(|| {
        env.iter().find_map(|name| std::env::var(name).ok().filter(|key| !key.trim().is_empty()))
    });
    let Some(key) = key else {
        if explicit.is_some() {
            bail!(
                "--embedder {choice} needs a key: set {}, or choose --embedder local or lexical",
                env.join(" or ")
            );
        }
        return local(false);
    };
    let built = match choice {
        EmbedderArg::Gemini => adk_harness::RagEmbedder::gemini(&key)
            .map(|embedder| (embedder, "Gemini (gemini-embedding-2)")),
        _ => adk_harness::RagEmbedder::openai(&key)
            .map(|embedder| (embedder, "OpenAI (text-embedding-3-small)")),
    };
    match built {
        Ok((embedder, name)) => Ok(Some((Arc::new(embedder), name.to_string()))),
        Err(error) if explicit.is_some() => bail!("--embedder {choice}: {error}"),
        Err(_) => local(false),
    }
}

/// The local ONNX model, or lexical recall with a note when it cannot load.
fn local(explicit: bool) -> Result<Option<(Arc<adk_harness::RagEmbedder>, String)>> {
    #[cfg(feature = "memory-local")]
    {
        let _ = explicit;
        match adk_harness::RagEmbedder::local() {
            Ok(embedder) => Ok(Some((Arc::new(embedder), LOCAL.to_string()))),
            Err(error) => {
                println!(
                    "note: the local embedding model is unavailable ({error}); facts are found \
                     by shared words"
                );
                Ok(None)
            }
        }
    }
    #[cfg(not(feature = "memory-local"))]
    {
        let _ = LOCAL;
        if explicit {
            bail!(
                "the local embedder is not compiled into this adk-cli build. Reinstall with \
                 `--features memory-local`, or choose --embedder lexical"
            );
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The order of spec 3.1 §7.32 (§13 Q9 decided): the provider's own embedder, else the
    /// local model; another provider only when chosen.
    #[test]
    fn each_chat_provider_gets_its_own_embedder_or_the_local_one() {
        let cases = [
            (Provider::Model(ModelProvider::Gemini), EmbedderArg::Gemini),
            (Provider::Model(ModelProvider::Openai), EmbedderArg::Openai),
            (Provider::Model(ModelProvider::Anthropic), EmbedderArg::Local),
            (Provider::Model(ModelProvider::Deepseek), EmbedderArg::Local),
            (Provider::Model(ModelProvider::Groq), EmbedderArg::Local),
            (Provider::Model(ModelProvider::Ollama), EmbedderArg::Local),
            (Provider::MistralRs, EmbedderArg::Local),
        ];
        for (provider, expected) in cases {
            assert_eq!(choose(Some(provider), None), expected, "{provider}");
        }
        assert_eq!(choose(None, None), EmbedderArg::Lexical, "the scripted model");
        for explicit in
            [EmbedderArg::Local, EmbedderArg::Gemini, EmbedderArg::Openai, EmbedderArg::Lexical]
        {
            assert_eq!(
                choose(Some(Provider::Model(ModelProvider::Anthropic)), Some(explicit)),
                explicit
            );
        }
    }
}
