//! # cargo-adk
//!
//! `cargo adk` scaffolds, builds, evaluates and deploys ADK-Rust projects. Install the
//! subcommand with `cargo install cargo-adk`; the
//! [README](https://github.com/zavora-ai/adk-rust/blob/main/cargo-adk/README.md) documents
//! every command, template, addon and pattern.
//!
//! This library is the scaffolding engine behind `cargo adk new`:
//!
//! | Module | Contents |
//! |--------|----------|
//! | [`registry`] | The template registry: every template, addon and pattern |
//! | [`template`], [`addon`], [`pattern`], [`provider`] | Templates, capability addons, enterprise patterns and model providers |
//! | [`composition`], [`codegen`] | The composition pipeline and the code it generates |
//! | [`autonomous_template`], [`website_template`], [`vision_template`] | The `autonomous`, `website` and `vision` templates |
//!
//! ## Features
//!
//! Each feature adds a subcommand or target to the `cargo-adk` binary; the library is the
//! same with or without them. No feature is on by default.
//!
//! | Feature | Enables |
//! |---------|---------|
//! | `bench` | `cargo adk bench`: performance benchmarks against real LLM APIs through `adk-bench` |
//! | `gcp` | `cargo adk deploy --target agent-engine` |
//! | `aws` | `cargo adk deploy --target ecs-express` |
//! | `azure` | `cargo adk deploy --target azure-container-apps` |

pub mod addon;
pub mod autonomous_template;
pub mod cli;
pub mod codegen;
pub mod composition;
pub mod interactive;
pub mod pattern;
pub mod provider;
pub mod registry;
pub mod template;
pub mod vision_template;
pub mod website_template;
