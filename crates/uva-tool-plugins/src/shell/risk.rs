//! Command-level risk classification.
//!
//! Kept beside the tool rather than in the transport file: the classifier
//! is the security decision, and the argv/redirect/compound rules are
//! easier to audit in one focused place.

use uva_tool::Risk;

/// Classify a whole shell command line.
pub(crate) fn classify_command_risk(command: &str) -> Risk {
    let command = command.trim();
    if command.is_empty() {
        return Risk::Unknown;
    }
    // A redirect writes a file (or feeds one); command substitution and
    // backticks hide a second command from this analysis. Neither is
    // safe to assume read-only.
    if command.contains('>')
        || command.contains('<')
        || command.contains("$(")
        || command.contains('`')
    {
        return Risk::High;
    }
    let mut unknown = false;
    for segment in shell_segments(command) {
        match classify_segment_risk(segment) {
            Risk::High => return Risk::High,
            // A segment this analyser cannot resolve, in a compound
            // construct it also cannot resolve, is not a command to
            // run unapproved: fail closed.
            Risk::Unknown if has_shell_control_syntax(segment) => {
                return Risk::High;
            }
            Risk::Unknown => unknown = true,
            Risk::Low => {}
        }
    }
    if unknown { Risk::Unknown } else { Risk::Low }
}

// Split on the shell operators that start a new command. Every segment
// must be safe for the whole command to be safe.
fn shell_segments(command: &str) -> Vec<&str> {
    let bytes = command.as_bytes();
    let mut segments = Vec::new();
    let mut start = 0;
    let mut idx = 0;
    while idx < bytes.len() {
        if matches!(bytes[idx], b';' | b'\n' | b'|' | b'&') {
            let segment = command[start..idx].trim();
            if !segment.is_empty() {
                segments.push(segment);
            }
            idx += 1;
            if idx < bytes.len() && matches!(bytes[idx], b'&' | b'|') {
                idx += 1;
            }
            start = idx;
        } else {
            idx += 1;
        }
    }
    let tail = command[start..].trim();
    if !tail.is_empty() {
        segments.push(tail);
    }
    segments
}

// Programs that only read their inputs. A command is low risk only
// when every segment it contains is on this list (or matches a
// read-only special case below); one unknown segment makes the whole
// command unknown, and one high-risk segment makes it high.
const READ_ONLY_COMMANDS: &[&str] = &[
    "ls",
    "cat",
    "head",
    "tail",
    "pwd",
    "echo",
    "printf",
    "grep",
    "rg",
    "wc",
    "sort",
    "uniq",
    "cut",
    "tr",
    "stat",
    "file",
    "du",
    "df",
    "date",
    "whoami",
    "id",
    "uname",
    "hostname",
    "which",
    "printenv",
    "true",
    "false",
    "test",
    "basename",
    "dirname",
    "realpath",
    "readlink",
    "tree",
    "ps",
    "diff",
    "cmp",
    "jq",
    "yq",
    "column",
    "less",
    "more",
    "nl",
    "od",
    "hexdump",
    "sha256sum",
    "md5sum",
    "base64",
    "sleep",
    "history",
    "alias",
    "type",
    "locate",
];

// Programs whose ordinary invocation writes, deletes, escalates or
// reaches the network. `find`, `sed` and `sort` are argument-sensitive
// and classified separately.
const HIGH_RISK_COMMANDS: &[&str] = &[
    "rm",
    "rmdir",
    "mv",
    "cp",
    "touch",
    "mkdir",
    "ln",
    "chmod",
    "chown",
    "chgrp",
    "install",
    "dd",
    "truncate",
    "tee",
    "shred",
    "kill",
    "pkill",
    "killall",
    "reboot",
    "shutdown",
    "halt",
    "poweroff",
    "mount",
    "umount",
    "sudo",
    "su",
    "doas",
    "passwd",
    "useradd",
    "usermod",
    "userdel",
    "groupadd",
    "crontab",
    "systemctl",
    "service",
    "pip",
    "pip3",
    "npm",
    "pnpm",
    "yarn",
    "gem",
    "apt",
    "apt-get",
    "yum",
    "dnf",
    "apk",
    "brew",
    "docker",
    "podman",
    "kubectl",
    "helm",
    "terraform",
    "curl",
    "wget",
    "nc",
    "ncat",
    "netcat",
    "ssh",
    "scp",
    "sftp",
    "rsync",
    "ftp",
    "telnet",
    "ping",
    "sh",
    "bash",
    "zsh",
    "fish",
    "dash",
    "ksh",
    "python",
    "python3",
    "perl",
    "ruby",
    "node",
    "deno",
    "bun",
    "php",
    "lua",
    "xargs",
    "eval",
    "awk",
    "gawk",
    "mawk",
    "nawk",
    "source",
];

fn classify_segment_risk(segment: &str) -> Risk {
    let words: Vec<&str> = segment.split_whitespace().collect();
    let Some(idx) = command_index(&words) else {
        return Risk::Unknown;
    };
    let program = words[idx];
    let name = program.rsplit('/').next().unwrap_or(program);
    let args = &words[idx + 1..];
    match name {
        "git" => classify_git_risk(args),
        "find" => {
            if args.iter().any(|arg| {
                matches!(
                    *arg,
                    "-delete"
                        | "-exec"
                        | "-execdir"
                        | "-ok"
                        | "-okdir"
                        | "-fprintf"
                        | "-fprint"
                        | "-fprint0"
                        | "-fls"
                )
            }) {
                Risk::High
            } else {
                Risk::Low
            }
        }
        "sed" => {
            if args.iter().any(|arg| is_sed_in_place(arg)) {
                Risk::High
            } else {
                Risk::Low
            }
        }
        "sort" => {
            if args
                .iter()
                .any(|arg| *arg == "-o" || arg.starts_with("--output"))
            {
                Risk::High
            } else {
                Risk::Low
            }
        }
        _ if name.starts_with("mkfs") => Risk::High,
        _ if HIGH_RISK_COMMANDS.contains(&name) => Risk::High,
        _ if READ_ONLY_COMMANDS.contains(&name) => Risk::Low,
        _ => Risk::Unknown,
    }
}

// The index of the program a segment runs: environment assignments,
// reserved words and transparent wrappers are skipped. A wrapper's
// own options (and its leading operands, e.g. `timeout`'s duration)
// are consumed so the index lands on the command it wraps.
fn command_index(words: &[&str]) -> Option<usize> {
    let mut idx = 0;
    loop {
        while idx < words.len() {
            if is_env_assignment(words[idx]) {
                idx += 1;
                continue;
            }
            // These words separate one command from the next; they
            // are never the program a segment runs.
            if matches!(
                words[idx],
                "then" | "do" | "else" | "elif" | "fi" | "done" | "{" | "}"
            ) {
                idx += 1;
                continue;
            }
            break;
        }
        if idx >= words.len() {
            return None;
        }
        match words[idx] {
            "command" | "builtin" | "exec" => idx += 1,
            "env" => {
                idx += 1;
                idx = skip_options(
                    words,
                    idx,
                    &["-u", "--unset", "-C", "--chdir"],
                );
            }
            // Wrappers run their arguments as the real command; their
            // own options and operands are not the program.
            "timeout" => {
                idx = skip_options(
                    words,
                    idx + 1,
                    &["-s", "--signal", "-k", "--kill-after"],
                );
                // DURATION is the wrapper's one operand.
                idx += usize::from(idx < words.len());
            }
            "nice" => {
                idx = skip_options(words, idx + 1, &["-n", "--adjustment"]);
            }
            "nohup" => idx += 1,
            "time" => {
                idx = skip_options(
                    words,
                    idx + 1,
                    &["-o", "--output", "-f", "--format"],
                );
            }
            "stdbuf" => {
                idx = skip_options(
                    words,
                    idx + 1,
                    &["-i", "--input", "-o", "--output", "-e", "--error"],
                );
            }
            "ionice" => {
                idx = skip_options(
                    words,
                    idx + 1,
                    &[
                        "-c",
                        "--class",
                        "-n",
                        "--classdata",
                        "-p",
                        "--pid",
                        "-P",
                        "--pgid",
                        "-u",
                        "--uid",
                    ],
                );
            }
            "chroot" => {
                idx = skip_options(words, idx + 1, &["--userspec", "--groups"]);
                // NEWROOT is the wrapper's one operand.
                idx += usize::from(idx < words.len());
            }
            _ => return Some(idx),
        }
    }
}

// Skip a wrapper's option words starting at `idx`, consuming a value
// for each option named in `takes_value` (and for `--name=value`
// spellings, which carry their own value).
fn skip_options(words: &[&str], mut idx: usize, takes_value: &[&str]) -> usize {
    while idx < words.len() && words[idx].starts_with('-') && words[idx] != "-"
    {
        let word = words[idx];
        let option = word.split_once('=').map(|(name, _)| name).unwrap_or(word);
        let consumes_next = word == option && takes_value.contains(&option);
        idx += if consumes_next { 2 } else { 1 };
    }
    idx
}

// Words and characters that mark a compound shell construct this
// analyser does not model. A segment that resolves to neither a known
// program nor a known wrapper, and still carries one, is not a
// read-only command.
fn has_shell_control_syntax(segment: &str) -> bool {
    segment.split_whitespace().any(|word| {
        matches!(
            word,
            "if" | "then"
                | "else"
                | "elif"
                | "fi"
                | "for"
                | "while"
                | "until"
                | "do"
                | "done"
                | "case"
                | "esac"
                | "select"
                | "in"
                | "function"
                | "!"
        )
    }) || segment
        .chars()
        .any(|ch| matches!(ch, '{' | '}' | '(' | ')'))
}

// `sed -Ei`, `sed -ni`, `sed -i.bak` and `--in-place` all write.
fn is_sed_in_place(arg: &str) -> bool {
    arg.starts_with("--in-place")
        || (arg.starts_with('-')
            && !arg.starts_with("--")
            && arg[1..].contains('i'))
}

fn is_env_assignment(word: &str) -> bool {
    let Some((key, _)) = word.split_once('=') else {
        return false;
    };
    !key.is_empty()
        && key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn classify_git_risk(args: &[&str]) -> Risk {
    // `--output=<file>` writes a file even under a read-only
    // subcommand such as `log`.
    if args.iter().any(|arg| arg.starts_with("--output")) {
        return Risk::High;
    }
    // Skip the global options that precede the subcommand; `-C` and
    // `-c` consume a value.
    let mut idx = 0;
    while idx < args.len() && args[idx].starts_with('-') {
        let takes_value =
            matches!(args[idx], "-C" | "-c" | "--git-dir" | "--work-tree");
        idx += if takes_value { 2 } else { 1 };
    }
    let Some(subcommand) = args.get(idx) else {
        return Risk::Unknown;
    };
    let rest = &args[idx + 1..];
    match *subcommand {
        "status" | "log" | "diff" | "show" | "rev-parse" | "describe"
        | "ls-files" | "ls-tree" | "grep" | "blame" | "cat-file"
        | "shortlog" | "rev-list" | "whatchanged" | "reflog" | "help"
        | "version" => Risk::Low,
        "commit" | "push" | "pull" | "fetch" | "checkout" | "switch"
        | "reset" | "clean" | "rm" | "mv" | "add" | "merge" | "rebase"
        | "cherry-pick" | "revert" | "apply" | "am" | "init" | "clone"
        | "submodule" | "worktree" | "bisect" | "gc" | "prune"
        | "filter-branch" => Risk::High,
        "branch" | "tag" => {
            let mutating = rest.iter().any(|arg| {
                matches!(
                    *arg,
                    "-d" | "-D"
                        | "-m"
                        | "-M"
                        | "-c"
                        | "-C"
                        | "-f"
                        | "--delete"
                        | "--move"
                        | "--copy"
                        | "--force"
                        | "--edit-description"
                )
            });
            if mutating { Risk::High } else { Risk::Low }
        }
        "remote" => {
            let mutating = rest.iter().any(|arg| {
                matches!(
                    *arg,
                    "add"
                        | "remove"
                        | "rm"
                        | "rename"
                        | "set-url"
                        | "set-head"
                        | "prune"
                        | "update"
                )
            });
            if mutating { Risk::High } else { Risk::Low }
        }
        "config" => {
            let reads = rest.iter().any(|arg| {
                matches!(*arg, "--get" | "--get-all" | "--list" | "-l")
            });
            if reads || rest.is_empty() {
                Risk::Low
            } else {
                Risk::High
            }
        }
        "stash" => {
            if matches!(rest.first().copied(), Some("list" | "show")) {
                Risk::Low
            } else {
                Risk::High
            }
        }
        _ => Risk::Unknown,
    }
}
