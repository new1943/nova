//! Security validators for bash commands.
//!
//! Ported from Claude Code bashSecurity.ts

use crate::bash::security::constants::{
    COMMAND_SUBSTITUTION_PATTERNS, ZSH_DANGEROUS_COMMANDS,
};
use crate::bash::security::quote::QuoteState;

/// Validation error with context
#[derive(Debug)]
pub struct ValidationError {
    pub message: String,
}

/// Main entry point: validate a bash command
pub fn validate_bash_command(command: &str) -> Result<(), ValidationError> {
    let trimmed = command.trim();

    if trimmed.is_empty() {
        return Err(ValidationError {
            message: "Empty command".to_string(),
        });
    }

    validate_command_substitution(trimmed)?;
    validate_output_redirection(trimmed)?;
    validate_zsh_dangerous(trimmed)?;
    validate_git_commit(trimmed)?;
    validate_proc_environ(trimmed)?;
    validate_brace_expansion(trimmed)?;
    validate_control_chars(trimmed)?;
    validate_ifs_injection(trimmed)?;
    validate_backslash_escaped_operators(trimmed)?;
    validate_jq_command(trimmed)?;
    validate_network_commands(trimmed)?;
    validate_command_structure(trimmed)?;
    validate_heredoc(trimmed)?;
    validate_find_command(trimmed)?;
    validate_tar_command(trimmed)?;
    validate_awk_sed_command(trimmed)?;
    validate_env_injection(trimmed)?;
    validate_env_manipulation(trimmed)?;
    validate_encoded_commands(trimmed)?;
    validate_hidden_chars(trimmed)?;
    validate_scripting_languages(trimmed)?;
    validate_pipe_depth(trimmed)?;

    Ok(())
}

fn validate_command_substitution(command: &str) -> Result<(), ValidationError> {
    for (pattern, description) in COMMAND_SUBSTITUTION_PATTERNS {
        if command.contains(pattern) {
            let (with_double, _) = QuoteState::extract_unquoted(command);
            if with_double.contains(pattern) {
                return Err(ValidationError {
                    message: format!("Blocked: {} — not allowed", description),
                });
            }
        }
    }

    let mut state = QuoteState::new();
    for c in command.chars() {
        let was_escaped = state.is_escaped();
        state.update(c);

        if c == '`' && !state.is_quoted() && !was_escaped {
            return Err(ValidationError {
                message: "Backtick command substitution is not allowed".to_string(),
            });
        }
    }

    Ok(())
}

fn validate_output_redirection(command: &str) -> Result<(), ValidationError> {
    let fully_unquoted = QuoteState::extract_unquoted(command).1;

    let redirect_patterns = [
        "/etc/passwd", "/etc/shadow", "/etc/sudoers", "/root",
        "/.ssh/", "/.bashrc", "/.bash_profile", "/.zshrc",
    ];

    for pattern in redirect_patterns {
        let search = format!("> {}", pattern);
        if fully_unquoted.contains(&search) {
            return Err(ValidationError {
                message: format!("Output redirection to '{}' is not allowed", pattern),
            });
        }
    }

    if fully_unquoted.contains("&>") || fully_unquoted.contains(">&") {
        if fully_unquoted.contains("2>&1") {
            // OK
        } else if fully_unquoted.contains("&>") || fully_unquoted.contains(">&") {
            let re = regex::Regex::new(r"&\d*>&?\d*").unwrap();
            for cap in re.find_iter(&fully_unquoted) {
                let m = cap.as_str();
                if m != "2>&1" && m != ">&2" {
                    return Err(ValidationError {
                        message: format!("Dangerous file descriptor redirection '{}' is not allowed", m),
                    });
                }
            }
        }
    }

    Ok(())
}

fn validate_zsh_dangerous(command: &str) -> Result<(), ValidationError> {
    let first_word = command
        .split_whitespace()
        .next()
        .unwrap_or("")
        .split('/')
        .next_back()
        .unwrap_or("");

    let first_lower = first_word.to_lowercase();

    for zsh_cmd in ZSH_DANGEROUS_COMMANDS {
        if first_lower == *zsh_cmd {
            return Err(ValidationError {
                message: format!("Zsh command '{}' is not allowed for security reasons", zsh_cmd),
            });
        }
    }

    let unquoted = QuoteState::extract_unquoted(command).1;
    if unquoted.contains("zmodload") && !in_single_quoted(command, "zmodload") {
        return Err(ValidationError {
            message: "zmodload is not allowed for security reasons".to_string(),
        });
    }

    Ok(())
}

fn in_single_quoted(s: &str, pattern: &str) -> bool {
    let mut in_quote = false;
    for part in s.split('\'') {
        if in_quote && part.contains(pattern) {
            return true;
        }
        in_quote = !in_quote;
    }
    false
}

fn fully_unquoted(s: &str) -> String {
    QuoteState::extract_unquoted(s).1
}

fn validate_git_commit(command: &str) -> Result<(), ValidationError> {
    let first_word = command.split_whitespace().next().unwrap_or("");
    if first_word != "git" {
        return Ok(());
    }

    if command.contains("git commit") && command.contains("-m") {
        let unquoted = fully_unquoted(command);
        if unquoted.contains("git commit") && (unquoted.contains("$( ") || unquoted.contains("`")) {
            return Err(ValidationError {
                message: "git commit message cannot contain command substitution".to_string(),
            });
        }
    }

    Ok(())
}

fn validate_proc_environ(command: &str) -> Result<(), ValidationError> {
    let unquoted = fully_unquoted(command);

    if unquoted.contains("/proc/") && (unquoted.contains("/environ") || unquoted.contains("environ")) {
        let re = regex::Regex::new(r"/proc/([\d]+|self)/environ").unwrap();
        if re.is_match(&unquoted) {
            return Err(ValidationError {
                message: "Access to /proc/*/environ is not allowed (could expose environment variables)".to_string(),
            });
        }
    }

    Ok(())
}

fn validate_brace_expansion(command: &str) -> Result<(), ValidationError> {
    let unquoted = fully_unquoted(command);

    let mut brace_depth = 0;
    let mut escaped = false;
    let mut has_brace_expansion = false;

    for c in unquoted.chars() {
        if c == '\\' && !escaped {
            escaped = true;
            continue;
        }
        if c == '{' {
            brace_depth += 1;
        } else if c == '}' {
            if brace_depth == 0 {
                return Err(ValidationError {
                    message: "Unmatched closing brace".to_string(),
                });
            }
            brace_depth -= 1;
        } else if c == ',' && brace_depth > 0 {
            has_brace_expansion = true;
        }
        escaped = false;
    }

    if has_brace_expansion {
        return Err(ValidationError {
            message: "Brace expansion is not allowed".to_string(),
        });
    }

    if brace_depth > 0 {
        return Err(ValidationError {
            message: "Unbalanced braces detected — possible brace expansion attack".to_string(),
        });
    }

    Ok(())
}

fn validate_control_chars(command: &str) -> Result<(), ValidationError> {
    for (_i, c) in command.char_indices() {
        let code = c as u32;
        if code <= 0x1f && code != 0x09 && code != 0x0a && code != 0x0d {
            return Err(ValidationError {
                message: format!("Control character 0x{:02x} is not allowed", code),
            });
        }
    }

    if command.contains('\x7f') {
        return Err(ValidationError {
            message: "DEL character (0x7f) is not allowed".to_string(),
        });
    }

    Ok(())
}

fn validate_ifs_injection(command: &str) -> Result<(), ValidationError> {
    let unquoted = fully_unquoted(command);

    if unquoted.contains("$IFS") || unquoted.contains("${IFS") {
        return Err(ValidationError {
            message: "IFS variable manipulation is not allowed".to_string(),
        });
    }

    Ok(())
}

fn validate_backslash_escaped_operators(command: &str) -> Result<(), ValidationError> {
    let unquoted = fully_unquoted(command);

    let escaped_operators = [';', '|', '&', '<', '>'];

    let mut escaped = false;
    for c in unquoted.chars() {
        if c == '\\' {
            escaped = true;
            continue;
        }
        if escaped && escaped_operators.contains(&c) {
            return Err(ValidationError {
                message: format!("Escaped operator '\\{}' is not allowed", c),
            });
        }
        escaped = false;
    }

    Ok(())
}

fn validate_jq_command(command: &str) -> Result<(), ValidationError> {
    let first_word = command.split_whitespace().next().unwrap_or("");
    if first_word != "jq" {
        return Ok(());
    }

    let unquoted = fully_unquoted(command);

    let dangerous_flags = ["--run", "-e", "-f", "--from-file"];

    for flag in dangerous_flags {
        if unquoted.contains(flag) {
            return Err(ValidationError {
                message: format!("jq '{}' flag is not allowed (can execute arbitrary code)", flag),
            });
        }
    }

    Ok(())
}

fn validate_network_commands(command: &str) -> Result<(), ValidationError> {
    let unquoted = fully_unquoted(command);
    let first_word = command.split_whitespace().next().unwrap_or("");

    match first_word {
        "curl" | "wget" => {
            let sensitive_redirects = [
                "/etc/passwd", "/etc/shadow", "/etc/sudoers",
                "/root", "/.ssh/", "/.bashrc", "/.bash_profile",
            ];
            for pattern in sensitive_redirects {
                if unquoted.contains("-o") && unquoted.contains(pattern) {
                    return Err(ValidationError {
                        message: format!("{} output to '{}' is not allowed", first_word, pattern),
                    });
                }
                if unquoted.contains("--output") && unquoted.contains(pattern) {
                    return Err(ValidationError {
                        message: format!("{} output to '{}' is not allowed", first_word, pattern),
                    });
                }
            }

            if (unquoted.contains("--data-binary") || unquoted.contains("-d"))
                && (unquoted.contains("$(") || unquoted.contains("`"))
            {
                return Err(ValidationError {
                    message: format!("{} with inline data containing command substitution is not allowed", first_word),
                });
            }
        }
        "nc" | "netcat" | "ncat" => {
            if unquoted.contains("-e") || unquoted.contains("--exec") || unquoted.contains("-c") {
                return Err(ValidationError {
                    message: "netcat -e/-c (execute) is not allowed".to_string(),
                });
            }
        }
        "ssh" => {
            if unquoted.contains("ProxyCommand") || unquoted.contains("LocalCommand") {
                return Err(ValidationError {
                    message: "ssh ProxyCommand/LocalCommand is not allowed".to_string(),
                });
            }
            let re = regex::Regex::new(r"-o\s+\w+=").unwrap();
            if re.is_match(&unquoted) {
                let dangerous_opts = ["ProxyCommand", "LocalCommand", "ControlMaster", "ControlPath"];
                for opt in dangerous_opts {
                    if unquoted.contains(opt) {
                        return Err(ValidationError {
                            message: format!("ssh -o {} is not allowed", opt),
                        });
                    }
                }
            }
        }
        _ => {}
    }

    Ok(())
}

fn validate_command_structure(command: &str) -> Result<(), ValidationError> {
    let unquoted = fully_unquoted(command);

    let dangerous_patterns = [
        (r";\s*(rm\s+-rf|dd\s+|mkfs\s+|shutdown|reboot)", "dangerous command after semicolon"),
        (r"\|\s*(sh|bash|zsh)\s*$", "pipe to shell"),
        (r"&&\s*(rm\s+-rf|dd\s+|mkfs\s+)", "dangerous command after &&"),
        (r"\|\|.*(rm\s+-rf|dd\s+|mkfs)", "dangerous command after ||"),
        (r"2>&1\s*&", "stderr redirect to background"),
    ];

    for (pattern, desc) in dangerous_patterns {
        let re = regex::Regex::new(pattern).unwrap();
        if re.is_match(&unquoted) {
            return Err(ValidationError {
                message: format!("Dangerous command structure: {}", desc),
            });
        }
    }

    let nested_count = unquoted.matches("$(").count();
    if nested_count > 2 {
        return Err(ValidationError {
            message: "Nested command substitution depth exceeds limit".to_string(),
        });
    }

    Ok(())
}

fn validate_heredoc(command: &str) -> Result<(), ValidationError> {
    let unquoted = fully_unquoted(command);

    if unquoted.contains("$(cat <<") || unquoted.contains("`<<") {
        return Err(ValidationError {
            message: "Heredoc inside command substitution is not allowed".to_string(),
        });
    }

    if unquoted.contains("<<<") && unquoted.contains("$(") {
        return Err(ValidationError {
            message: "Here-string with command substitution is not allowed".to_string(),
        });
    }

    if unquoted.contains("<<-") {
        return Err(ValidationError {
            message: "Here-doc with tab stripping (<<-) is not allowed".to_string(),
        });
    }

    Ok(())
}

fn validate_find_command(command: &str) -> Result<(), ValidationError> {
    let first_word = command.split_whitespace().next().unwrap_or("");
    if first_word != "find" {
        return Ok(());
    }

    let unquoted = fully_unquoted(command);

    let dangerous_flags = ["-exec", "-execdir", "-ok", "-okdir", "-delete", "-prune"];
    for flag in dangerous_flags {
        if unquoted.contains(flag) {
            if flag == "-exec" && (unquoted.contains("{} \\;") || unquoted.contains("{};")) {
                return Err(ValidationError {
                    message: "find -exec with shell invocation is not allowed".to_string(),
                });
            }
            if flag != "-exec" {
                return Err(ValidationError {
                    message: format!("find {} is not allowed", flag),
                });
            }
        }
    }

    Ok(())
}

fn validate_tar_command(command: &str) -> Result<(), ValidationError> {
    let first_word = command.split_whitespace().next().unwrap_or("");
    if first_word != "tar" {
        return Ok(());
    }

    let unquoted = fully_unquoted(command);

    if unquoted.contains("--checkpoint") || unquoted.contains("--checkpoint-action") {
        return Err(ValidationError {
            message: "tar checkpoint actions are not allowed".to_string(),
        });
    }

    if unquoted.contains("-p") && unquoted.contains("/dev/") {
        return Err(ValidationError {
            message: "tar device extraction is not allowed".to_string(),
        });
    }

    if unquoted.contains("-xvf") && (unquoted.contains("http://") || unquoted.contains("https://") || unquoted.contains("ftp://")) {
        return Err(ValidationError {
            message: "tar remote archive extraction is not allowed".to_string(),
        });
    }

    Ok(())
}

fn validate_awk_sed_command(command: &str) -> Result<(), ValidationError> {
    let first_word = command.split_whitespace().next().unwrap_or("");
    if first_word != "awk" && first_word != "sed" && first_word != "gawk" {
        return Ok(());
    }

    if command.contains("system(") || command.contains("getline") {
        return Err(ValidationError {
            message: "AWK system() or getline is not allowed".to_string(),
        });
    }

    if command.contains("|") && (command.contains("/bin/sh") || command.contains("/bin/bash")) {
        return Err(ValidationError {
            message: "Sed piping to shell is not allowed".to_string(),
        });
    }

    let unquoted = fully_unquoted(command);
    if unquoted.contains("sed") && unquoted.contains("-e") && unquoted.contains(";") {
        return Err(ValidationError {
            message: "Sed command chaining with shell is not allowed".to_string(),
        });
    }

    Ok(())
}

fn validate_env_injection(command: &str) -> Result<(), ValidationError> {
    let unquoted = fully_unquoted(command);

    if unquoted.contains("env -i") {
        return Err(ValidationError {
            message: "env -i (clean environment) is not allowed".to_string(),
        });
    }

    let proxy_vars = ["HTTP_PROXY", "HTTPS_PROXY", "FTP_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "ftp_proxy", "all_proxy"];
    for var in proxy_vars {
        if unquoted.contains(&format!("{}=", var)) || unquoted.contains(&format!("export {}", var)) {
            return Err(ValidationError {
                message: "Proxy environment variable injection is not allowed".to_string(),
            });
        }
    }

    Ok(())
}

fn validate_env_manipulation(command: &str) -> Result<(), ValidationError> {
    let unquoted = fully_unquoted(command);

    let dangerous_vars = ["BASH_ENV", "ENV", "CDPATH", "MAIL", "MAILPATH", "HISTFILE", "HOSTFILE", "INPUTRC"];
    for var in dangerous_vars {
        if unquoted.contains(&format!("{}=", var)) || unquoted.contains(&format!("export {}", var)) {
            return Err(ValidationError {
                message: format!("Environment variable {} manipulation is not allowed", var),
            });
        }
    }

    Ok(())
}

fn validate_encoded_commands(command: &str) -> Result<(), ValidationError> {
    if (command.contains("base64 -d") || command.contains("base64 --decode")) && (command.contains("$(") || command.contains("`")) {
        return Err(ValidationError {
            message: "Base64 decode with command substitution is not allowed".to_string(),
        });
    }

    if command.contains("tr 'A-Za-z' 'N-ZA-Mn-za-m'") || command.contains("rot13") {
        return Err(ValidationError {
            message: "Rot13 encoding execution is not allowed".to_string(),
        });
    }

    let unquoted = fully_unquoted(command);
    if unquoted.contains("xxd -r") || (command.contains("printf") && command.contains("\\x")) {
        return Err(ValidationError {
            message: "Hex encoded command execution is not allowed".to_string(),
        });
    }

    Ok(())
}

fn validate_hidden_chars(command: &str) -> Result<(), ValidationError> {
    let hidden_chars = ['\u{200B}', '\u{200C}', '\u{200D}', '\u{FEFF}'];
    for c in command.chars() {
        if hidden_chars.contains(&c) {
            return Err(ValidationError {
                message: "Hidden unicode characters (zero-width) are not allowed".to_string(),
            });
        }
    }

    let direction_chars = ['\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}'];
    for c in command.chars() {
        if direction_chars.contains(&c) {
            return Err(ValidationError {
                message: "Text direction override characters are not allowed".to_string(),
            });
        }
    }

    Ok(())
}

fn validate_scripting_languages(command: &str) -> Result<(), ValidationError> {
    let first_word = command.split_whitespace().next().unwrap_or("");

    if first_word == "python" || first_word == "python3" || first_word == "python2" || first_word == "perl" || first_word == "ruby" || first_word == "php" || first_word == "node" {
        if command.contains(" -c ") || command.contains(" -c\n") {
            return Err(ValidationError {
                message: "Script interpreter -c (compile) mode is not allowed".to_string(),
            });
        }

        if first_word == "perl" && command.contains("exec") {
            return Err(ValidationError {
                message: "Perl exec is not allowed".to_string(),
            });
        }

        if first_word == "ruby" && command.contains("-e ") {
            return Err(ValidationError {
                message: "Ruby -e (inline code) is not allowed".to_string(),
            });
        }

        if first_word == "php" && command.contains("-r ") {
            return Err(ValidationError {
                message: "PHP -r (inline code) is not allowed".to_string(),
            });
        }
    }

    Ok(())
}

fn validate_pipe_depth(command: &str) -> Result<(), ValidationError> {
    let unquoted = fully_unquoted(command);

    let pipe_count = unquoted.matches('|').count();
    if pipe_count > 5 {
        return Err(ValidationError {
            message: "Pipe chain depth exceeds limit (max 5)".to_string(),
        });
    }

    let trimmed = unquoted.trim();
    if trimmed.ends_with("| sh") || trimmed.ends_with("| bash") || trimmed.ends_with("| zsh") {
        return Err(ValidationError {
            message: "Piping directly to shell is not allowed".to_string(),
        });
    }

    if unquoted.contains("2>&1") && unquoted.contains("|") {
        return Err(ValidationError {
            message: "Stderr redirect in pipe chain is not allowed".to_string(),
        });
    }

    Ok(())
}
