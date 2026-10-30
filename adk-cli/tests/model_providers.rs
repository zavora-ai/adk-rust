//! `--provider` for every provider adk-model has its own client for (#1219): the binary builds
//! the provider's client when its feature is compiled in, and otherwise refuses and names the
//! feature to reinstall with. No request reaches a provider: `chat` ends at the closed input.

#![cfg(unix)]

mod common;

use std::process::{Command, Output, Stdio};

/// An Azure AI Inference endpoint that is never contacted.
const AZURE_ENDPOINT: &str = "https://example.services.ai.azure.com/models";

/// `adk-rust <flags> chat --ephemeral` with closed input, a private home, no provider
/// environment, and `env` set.
fn chat(flags: &[&str], env: &[(&str, &str)]) -> Output {
    let home = tempfile::tempdir().unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_adk-rust"));
    command.args(flags).args(["chat", "--ephemeral"]).current_dir(home.path()).stdin(Stdio::null());
    common::isolate(&mut command, home.path());
    for name in
        ["AZURE_AI_ENDPOINT", "AWS_REGION", "AWS_DEFAULT_REGION", "AWS_BEARER_TOKEN_BEDROCK"]
    {
        command.env_remove(name);
    }
    command.envs(env.iter().copied()).output().expect("adk-rust runs")
}

#[test]
fn each_provider_builds_its_client_or_names_its_feature() {
    for (provider, model, feature, compiled) in [
        ("muse", None, "muse", cfg!(feature = "muse")),
        ("openrouter", None, "openrouter", cfg!(feature = "openrouter")),
        ("opencode", None, "opencode", cfg!(feature = "opencode")),
        ("opencode-go", None, "opencode", cfg!(feature = "opencode")),
        ("bedrock", Some("us.anthropic.claude-sonnet-4-6"), "bedrock", cfg!(feature = "bedrock")),
        ("azure-ai", Some("gpt-5.5"), "azure-ai", cfg!(feature = "azure-ai")),
    ] {
        let mut flags = vec!["--provider", provider, "--api-key", "test-key-not-sent"];
        if let Some(model) = model {
            flags.extend(["--model", model]);
        }
        let output = chat(&flags, &[("AZURE_AI_ENDPOINT", AZURE_ENDPOINT)]);
        let text = common::text(&output);
        if compiled {
            assert!(output.status.success(), "{provider}: {text}");
        } else {
            assert!(!output.status.success(), "{provider}: {text}");
            assert!(
                text.contains(&format!("Reinstall with `--features {feature}`")),
                "{provider}: {text}"
            );
        }
    }
}

#[test]
fn deployment_scoped_providers_need_a_model_and_azure_its_endpoint() {
    for (provider, hint, compiled) in [
        (
            "bedrock",
            "Amazon Bedrock has no default model: pass --model <a Bedrock model or inference \
             profile id>",
            cfg!(feature = "bedrock"),
        ),
        (
            "azure-ai",
            "Azure AI Inference has no default model: pass --model <the model deployed at \
             AZURE_AI_ENDPOINT>",
            cfg!(feature = "azure-ai"),
        ),
    ] {
        let output = chat(&["--provider", provider, "--api-key", "test-key-not-sent"], &[]);
        let text = common::text(&output);
        assert!(!output.status.success(), "{provider}: {text}");
        let expected = if compiled { hint.to_string() } else { format!("--features {provider}") };
        assert!(text.contains(&expected), "{provider}: {text}");
    }

    if cfg!(feature = "azure-ai") {
        let flags = ["--provider", "azure-ai", "--model", "gpt-5.5", "--api-key", "test-key"];
        let output = chat(&flags, &[]);
        let text = common::text(&output);
        assert!(!output.status.success(), "{text}");
        assert!(text.contains("set AZURE_AI_ENDPOINT"), "{text}");
    }
}

#[test]
fn an_opencode_service_refuses_a_model_it_does_not_route() {
    let flags =
        ["--provider", "opencode-go", "--model", "claude-sonnet-5", "--api-key", "test-key"];
    let output = chat(&flags, &[]);
    let text = common::text(&output);
    assert!(!output.status.success(), "{text}");
    let expected = if cfg!(feature = "opencode") {
        "OpenCode Go does not route model 'claude-sonnet-5'"
    } else {
        "Reinstall with `--features opencode`"
    };
    assert!(text.contains(expected), "{text}");
}

#[test]
fn an_openai_compatible_preset_is_not_a_provider() {
    let output = chat(&["--provider", "xai"], &[]);
    let text = common::text(&output);
    assert!(!output.status.success(), "{text}");
    assert!(text.contains("unsupported provider: xai"), "{text}");
    assert!(text.contains("muse, openrouter, opencode, opencode-go, bedrock, azure-ai"), "{text}");
}
