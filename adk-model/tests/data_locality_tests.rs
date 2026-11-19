//! `Llm::data_locality` for the clients that can run on this machine (spec 3.7 X6): an
//! OpenAI-compatible or Ollama server at a loopback address is `Loopback`, everything else
//! `Remote`, and a fallback chain is as local as its least local model.

use std::sync::Arc;

use adk_core::{DataLocality, Llm};
use adk_model::{FallbackLlm, ScriptedLlm, ScriptedResponse};

fn scripted(locality: DataLocality) -> Arc<dyn Llm> {
    Arc::new(
        ScriptedLlm::new("scripted", Vec::<ScriptedResponse>::new()).with_data_locality(locality),
    )
}

#[test]
fn a_fallback_chain_is_as_local_as_its_least_local_model() {
    let chain = |models: [DataLocality; 2]| {
        FallbackLlm::builder(scripted(models[0]))
            .fallback(scripted(models[1]))
            .build()
            .data_locality()
    };
    assert_eq!(chain([DataLocality::InProcess, DataLocality::InProcess]), DataLocality::InProcess);
    assert_eq!(chain([DataLocality::InProcess, DataLocality::Loopback]), DataLocality::Loopback);
    assert_eq!(chain([DataLocality::InProcess, DataLocality::Remote]), DataLocality::Remote);
}

#[test]
fn a_scripted_model_is_remote_unless_told_otherwise() {
    let model = ScriptedLlm::new("claude-sonnet-5-5", Vec::<ScriptedResponse>::new());
    assert_eq!(model.data_locality(), DataLocality::Remote);
}

#[cfg(feature = "openai")]
mod openai {
    use adk_core::{DataLocality, Llm};
    use adk_model::openai::{
        OpenAIClient, OpenAIConfig, OpenAIResponsesClient, OpenAIResponsesConfig,
    };
    use adk_model::{OpenAICompatible, OpenAICompatibleConfig};

    #[test]
    fn an_openai_compatible_server_at_a_loopback_address_is_loopback() {
        let local = OpenAICompatible::new(
            OpenAICompatibleConfig::new("unused", "qwen3-vl-8b")
                .with_base_url("http://127.0.0.1:8000/v1"),
        )
        .unwrap();
        assert_eq!(local.data_locality(), DataLocality::Loopback);
        let hosted =
            OpenAICompatible::new(OpenAICompatibleConfig::together("key", "llama-4")).unwrap();
        assert_eq!(hosted.data_locality(), DataLocality::Remote);
        let openai = OpenAICompatible::new(OpenAICompatibleConfig::new("key", "gpt-5.6")).unwrap();
        assert_eq!(openai.data_locality(), DataLocality::Remote);
    }

    #[test]
    fn openai_clients_follow_their_base_url() {
        let local = OpenAIClient::new(OpenAIConfig::compatible(
            "unused",
            "http://localhost:1234/v1",
            "qwen3-vl-8b",
        ))
        .unwrap();
        assert_eq!(local.data_locality(), DataLocality::Loopback);
        let hosted = OpenAIClient::new(OpenAIConfig::new("key", "gpt-5.6")).unwrap();
        assert_eq!(hosted.data_locality(), DataLocality::Remote);

        let mut config = OpenAIResponsesConfig::new("unused", "qwen3-vl-8b");
        config.base_url = Some("http://[::1]:8000/v1".to_string());
        config.open_responses_mode = Some(true);
        let responses = OpenAIResponsesClient::new(config).unwrap();
        assert_eq!(responses.data_locality(), DataLocality::Loopback);
    }
}

#[cfg(feature = "ollama")]
mod ollama {
    use adk_core::{DataLocality, Llm};
    use adk_model::ollama::{OllamaConfig, OllamaModel};

    #[test]
    fn an_ollama_server_on_this_machine_is_loopback_unless_the_model_is_a_cloud_model() {
        let local = OllamaModel::new(OllamaConfig::new("qwen3-vl:8b")).unwrap();
        assert_eq!(local.data_locality(), DataLocality::Loopback);
        let cloud = OllamaModel::new(OllamaConfig::new("qwen3-vl:235b-cloud")).unwrap();
        assert_eq!(cloud.data_locality(), DataLocality::Remote, "forwarded to ollama.com");
        let lan = OllamaModel::new(OllamaConfig::with_host("http://10.0.0.7:11434", "qwen3-vl:8b"))
            .unwrap();
        assert_eq!(lan.data_locality(), DataLocality::Remote);
    }
}
