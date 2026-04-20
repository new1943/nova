//! Security constants — blocked patterns, dangerous commands, etc.

/// Always blocked — catastrophic / destructive patterns
pub const BLOCKED_PATTERNS: &[&str] = &[
    "rm -rf /",
    "rm -rf /*",
    "rm -rf ~",
    "mkfs",
    "dd if=",
    ":(){ :|:& };:",
    "shutdown",
    "reboot",
    "halt",
    "poweroff",
    "mv / ",
    "cp /dev/null",
];

/// Shell operators that are always blocked (both modes)
pub const BLOCKED_OPERATORS: &[&str] = &["$(", "`", "<(", ">("];

/// Command substitution patterns
pub const COMMAND_SUBSTITUTION_PATTERNS: &[(&str, &str)] = &[
    ("<(", "process substitution <()"),
    (">(", "process substitution >()"),
    ("=(", "Zsh process substitution =()"),
    ("$(", "$() command substitution"),
    ("${", "${} parameter substitution"),
    ("$[", "$[] legacy arithmetic expansion"),
    ("~[", "Zsh-style parameter expansion"),
    ("(e:", "Zsh-style glob qualifiers"),
    ("(+", "Zsh glob qualifier with command execution"),
];

/// Zsh-specific dangerous commands (that can bypass security checks)
/// These are checked against the base command
pub const ZSH_DANGEROUS_COMMANDS: &[&str] = &[
    // zmodload is the gateway to many dangerous module-based attacks
    "zmodload",
    // emulate with -c flag is an eval-equivalent
    "emulate",
    // Zsh module builtins
    "sysopen",
    "sysread",
    "syswrite",
    "sysseek",
    "zpty",
    "ztcp",
    "zsocket",
    "mapfile",
    // zsh/files builtins
    "zf_rm",
    "zf_mv",
    "zf_ln",
    "zf_chmod",
    "zf_chown",
    "zf_mkdir",
    "zf_rmdir",
    "zf_chgrp",
];

/// Sandboxed allowed command whitelist
pub const SANDBOX_ALLOWED: &[&str] = &[
    "ls", "cat", "find", "head", "tail", "wc", "diff", "stat",
    "echo", "pwd", "which", "env", "date", "whoami", "uname",
    "grep", "rg", "ag", "sed", "awk", "sort", "uniq", "tr",
    "file", "du", "df", "tree", "less", "more",
    "git", "cargo", "rustc", "python", "python3", "node",
    "mkdir", "touch", "cp", "mv", "rm",
];

/// Commands that are allowed in sandbox mode
pub fn is_sandbox_allowed(cmd: &str) -> bool {
    SANDBOX_ALLOWED.contains(&cmd)
}

/// Check if command is a Zsh dangerous command
pub fn is_zsh_dangerous(cmd: &str) -> bool {
    ZSH_DANGEROUS_COMMANDS.contains(&cmd)
}
