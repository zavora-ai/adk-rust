//! # cargo-adk library
//!
//! Core types and modules for the composable template scaffolding engine.
//! This library provides the Template Registry, Capability Addons, Enterprise Patterns,
//! Composition Pipeline, and Interactive Wizard for `cargo adk new`.

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
