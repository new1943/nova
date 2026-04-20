//! Bash security checks — Rust port of Claude Code bashSecurity.ts
//!
//! Implements 23 security checks for bash command validation.

pub mod constants;
pub mod quote;
pub mod validator;

pub use constants::*;
pub use quote::QuoteState;
pub use validator::validate_bash_command;
