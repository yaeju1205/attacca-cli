//! What the agent is told about this machine on a session this CLI started.
//!
//! This goes in `ZNewSession::preamble`: system instructions for the session alone, appended to the
//! agent's own on every turn. The agent keeps its identity, tools and skills; this tells it that the
//! `file_io` and `terminal` tools it can see point at the person it is talking to.
//!
//! It rode in front of the first message before `create_session_with` existed, which meant it applied
//! to one turn and had to be hidden from the transcript. As a preamble it applies to every turn and
//! is never part of the conversation, so [`SENTINEL`] and [`strip`] are now only for reading back
//! sessions an older build created.

/// Separated the brief from the user's message back when it was prepended to one. Retained because
/// [`strip`] needs it to read those sessions back; nothing writes it any more.
pub const SENTINEL: &str = "\n──── end of node brief; the user's message follows ────\n\n";

/// What this node offers, as the agent should hear it.
#[derive(Debug, Clone)]
pub struct NodeBrief {
    pub node_name: String,
    pub file_root: String,
    /// Whether `terminal` is announced. `ATTACCA_NO_TERMINAL` omits it, and promising a capability
    /// the node does not serve would just earn a `capability_not_announced`.
    pub terminal: bool,
}

impl NodeBrief {
    /// The brief, for `ZNewSession::preamble`.
    pub fn preamble(&self) -> String {
        let mut s = String::new();
        s.push_str(
            "You are talking with a person working in attacca-cli: a terminal client that runs as a \
             Zyris node on their own computer.\n\n",
        );
        s.push_str(&format!(
            "That node is \"{}\", and the capabilities it announces act on *that* machine — the one \
             this person is sitting at:\n\n",
            self.node_name
        ));
        s.push_str("- `file_io` — stat, list, read, write, remove, mkdir.\n");
        if self.terminal {
            s.push_str(
                "- `terminal` — `exec` for one-shot commands, plus `open`/`write`/`resize`/`close` \
                 for an interactive PTY.\n",
            );
        }
        s.push('\n');
        s.push_str(&format!(
            "The working directory for {} is:\n\n    {}\n\n",
            if self.terminal {
                "both of them"
            } else {
                "`file_io`"
            },
            self.file_root
        ));
        s.push_str(
            "A relative path resolves against that directory, so `src/main.rs` means the file of \
             that name inside it",
        );
        if self.terminal {
            s.push_str(", and `exec` starts each command there unless you pass a `cwd`");
        }
        s.push_str(
            ". Absolute paths are honoured as given and reach the rest of the filesystem, so use a \
             relative path when you mean the person's project and an absolute one only when you \
             genuinely mean elsewhere.\n\n",
        );
        s.push_str(
            "Use these by default. When this person says \"this file\", \"my project\", or \"run the \
             tests\", they mean on that node — not on any other machine on the account, and not \
             hypothetically. Its tools are the ones carrying its name; another node is another \
             computer, so do not reach for one by mistake.\n\n",
        );
        s.push_str(
            "Read before you write. Prefer `file_io` over shelling out to `cat`, `ls` or `sed`: it \
             reports failures properly instead of burying them in a shell's stderr.\n\n",
        );
        if self.terminal {
            s.push_str(
                "These calls run immediately, with no confirmation step in front of them, so treat \
                 anything destructive or irreversible as something to propose first and run only \
                 once the person has agreed.\n",
            );
        }
        s.push_str(SENTINEL);
        s
    }
}

/// Drop a brief from the front of a message for display.
///
/// Returns the text unchanged when there is no brief, so it is safe to run over every user message,
/// including history from sessions this CLI never touched.
pub fn strip(text: &str) -> &str {
    match text.split_once(SENTINEL) {
        Some((_, rest)) => rest.trim_start(),
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brief() -> NodeBrief {
        NodeBrief {
            node_name: "build-box".into(),
            file_root: "/home/ada/project".into(),
            terminal: true,
        }
    }

    #[test]
    fn the_brief_names_the_node_and_its_working_directory() {
        let text = brief().preamble();
        assert!(text.contains("build-box"));
        assert!(text.contains("/home/ada/project"));
        assert!(text.contains("working directory"));
        assert!(text.contains("file_io"));
        assert!(text.contains("terminal"));
        assert!(text.ends_with(SENTINEL));
    }

    /// With a terminal, the root is the cwd for *both* tools — `main` roots them together, so the
    /// brief may say so.
    #[test]
    fn the_working_directory_covers_both_tools_when_a_terminal_is_served() {
        let text = brief().preamble();
        assert!(text.contains("both of them"), "{text}");
        assert!(text.contains("`cwd`"), "exec's cwd override is worth knowing about");
    }

    /// Promising `terminal` when the node does not serve it would earn a `capability_not_announced`.
    #[test]
    fn a_node_without_a_terminal_does_not_advertise_one() {
        let text = NodeBrief {
            terminal: false,
            ..brief()
        }
        .preamble();
        assert!(!text.contains("`terminal`"));
        assert!(!text.contains("PTY"));
        assert!(!text.contains("both of them"));
        assert!(text.contains("file_io"));
        assert!(text.contains("/home/ada/project"));
    }

    /// The root stopped being a jail when `zyris-caps` moved to a shared `resolve_under`: `..` and
    /// absolute paths now pass through. Saying otherwise would have the agent trusting a boundary
    /// that is not there.
    #[test]
    fn the_brief_does_not_claim_a_confinement_that_no_longer_exists() {
        for text in [brief().preamble(), NodeBrief { terminal: false, ..brief() }.preamble()] {
            assert!(!text.to_lowercase().contains("confined"), "{text}");
            assert!(!text.to_lowercase().contains("refused"), "{text}");
            assert!(text.contains("Absolute paths are honoured"), "{text}");
        }
    }

    /// The point of the sentinel: the durable `user_message` event carries brief *and* message, and
    /// only the message may reach the screen.
    #[test]
    fn strip_recovers_only_the_users_own_text() {
        let sent = format!("{}{}", brief().preamble(), "what is in main.rs?");
        assert_eq!(strip(&sent), "what is in main.rs?");
    }

    #[test]
    fn strip_leaves_an_ordinary_message_alone() {
        assert_eq!(strip("just a message"), "just a message");
        assert_eq!(strip(""), "");
    }

    #[test]
    fn strip_keeps_a_multiline_message_intact() {
        let sent = format!("{}{}", brief().preamble(), "line one\n\nline two");
        assert_eq!(strip(&sent), "line one\n\nline two");
    }
}
