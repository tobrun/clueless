//! Prompt building: one system and one user message whose transcript part is
//! append-only, so the server's prefix cache keeps hitting (C-prompt-prefix-stable).

use clueless_types::{AssistProfile, Origin, Speaker};

use crate::store::TranscriptStore;

/// The system message shared by every profile (C-prompt-prefix-stable): who
/// is who, plain text, the length cap and the language rule. The answer shape
/// lives in the instruction at the end of the user message.
pub const BASE_RULES: &str = concat!(
    "You are a live meeting assistant. You see the transcript of a meeting in progress. ",
    "\"Me\" is the user you help. \"Them\" is the other side.\n",
    "Write plain text, no markdown. Use at most 60 words. ",
    "Answer in the language of the conversation.",
);

/// The answer shape sentence that ends the Manual and Interview instructions.
pub const ANSWER_SHAPE: &str = "Answer with one sentence that is the direct answer, then at most 3 short lines starting with a dash.";

/// The silence rule that ends the instruction of every automatic request.
pub const PASS_RULE: &str = "If there is nothing useful to add, reply with exactly: PASS";

const INTERVIEW_SENTENCE: &str = "If Them asked a question or raised a point that needs a reply from me, give me the answer to say.";

const BRAINSTORM_SENTENCE: &str = "I am talking. Give me up to 3 short lines starting with a dash: ideas, angles or facts that extend what I am saying and that I have not said yet.";

const PREVIOUS_ANSWER_HEADER: &str = "YOUR PREVIOUS ANSWER (already shown, add only what is new):";

/// How many characters of the previous answer the prompt carries.
const PREVIOUS_ANSWER_CHARS: usize = 600;

/// What a request asks for: the active profile, whether the user pressed the
/// key or the app decided, and the last answer shown (if any).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ask<'a> {
    pub profile: AssistProfile,
    pub origin: Origin,
    pub previous_answer: Option<&'a str>,
}

/// The result of [`build_for`]: the two messages plus the store's
/// `last_line_id` read under the same borrow, so the caller can store it as
/// the next `last_trigger_line_id` without a second look at the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Built {
    pub messages: Vec<PromptMessage>,
    pub last_line_id: u64,
}

/// One chat message as the prompt builder produces it. The engine converts
/// these to the LLM client's message type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptMessage {
    pub role: String,
    pub content: String,
}

/// The latest interim text of an utterance that is not yet committed or
/// dropped, with the open segment's time range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InProgressText {
    pub speaker: Speaker,
    pub t0_ms: u64,
    pub t1_ms: u64,
    pub text: String,
}

/// One transcript line: `[mm:ss] Me: text`, the time taken from the start.
pub fn format_line(t0_ms: u64, speaker: Speaker, text: &str) -> String {
    let total_secs = t0_ms / 1000;
    let minutes = total_secs / 60;
    let seconds = total_secs % 60;
    let who = match speaker {
        Speaker::Me => "Me",
        Speaker::Them => "Them",
    };
    format!("[{minutes:02}:{seconds:02}] {who}: {text}")
}

/// The stable, append-only part of the user message: the header, the summary
/// block when one exists, and one line per committed utterance. It only ever
/// grows between compressions.
pub fn transcript_part(store: &TranscriptStore) -> String {
    let mut parts = vec![String::from("TRANSCRIPT SO FAR:")];
    if let Some(summary) = store.summary() {
        parts.push(String::from("EARLIER IN THIS MEETING (summary):"));
        parts.push(summary.to_string());
    }
    for line in store.lines() {
        parts.push(format_line(
            line.utterance.t0_ms,
            line.utterance.id.speaker,
            &line.utterance.text,
        ));
    }
    parts.join("\n")
}

/// Build the two chat messages: the system message (`BASE_RULES`, plus the
/// notes under `ABOUT THE USER:` when set - identical for every profile) and
/// the user message (transcript part, a blank line, then the tail: the
/// in-progress block, the previous answer block and the instruction, separated
/// by blank lines).
///
/// `last_trigger_line_id` is the store's `last_line_id` at the previous
/// trigger (0 before the first one) and decides which Them lines the
/// instruction quotes.
pub fn build_for(
    store: &TranscriptStore,
    notes: Option<&str>,
    in_progress: &[InProgressText],
    last_trigger_line_id: u64,
    ask: &Ask,
) -> Built {
    let system = match notes {
        Some(text) => format!("{BASE_RULES}\n\nABOUT THE USER:\n{text}"),
        None => BASE_RULES.to_string(),
    };

    let mut tail_parts = Vec::new();
    if !in_progress.is_empty() {
        let mut block = String::from("IN PROGRESS (may be incomplete):");
        for entry in in_progress {
            block.push('\n');
            block.push_str(&format_line(entry.t0_ms, entry.speaker, &entry.text));
        }
        tail_parts.push(block);
    }
    if let Some(previous) = ask.previous_answer {
        let cut: String = previous.chars().take(PREVIOUS_ANSWER_CHARS).collect();
        tail_parts.push(format!("{PREVIOUS_ANSWER_HEADER}\n{cut}"));
    }
    tail_parts.push(instruction(store, in_progress, last_trigger_line_id, ask));

    let user = format!("{}\n\n{}", transcript_part(store), tail_parts.join("\n\n"));

    Built {
        messages: vec![
            PromptMessage {
                role: "system".to_string(),
                content: system,
            },
            PromptMessage {
                role: "user".to_string(),
                content: user,
            },
        ],
        last_line_id: store.last_line_id(),
    }
}

/// The last block of the user message, by profile and origin. Manual requests
/// never carry the PASS rule: an explicit request must produce an answer.
fn instruction(
    store: &TranscriptStore,
    in_progress: &[InProgressText],
    last_trigger_line_id: u64,
    ask: &Ask,
) -> String {
    let automatic = ask.origin == Origin::Auto;
    match (ask.profile, automatic) {
        (AssistProfile::Brainstorm, true) => format!("{BRAINSTORM_SENTENCE}\n{PASS_RULE}"),
        (AssistProfile::Brainstorm, false) => BRAINSTORM_SENTENCE.to_string(),
        (AssistProfile::Interview, true) => {
            let ask_line = match them_quote(store, in_progress, last_trigger_line_id) {
                Some(quote) => format!("The last thing Them said was: \"{quote}\".\n"),
                None => String::new(),
            };
            format!("{ask_line}{INTERVIEW_SENTENCE} {ANSWER_SHAPE}\n{PASS_RULE}")
        }
        (AssistProfile::Manual | AssistProfile::Interview, _) => {
            format!(
                "{}\n{ANSWER_SHAPE}",
                task_sentence(store, in_progress, last_trigger_line_id)
            )
        }
    }
}

/// The Them text since the last trigger (finals, then in-progress), joined
/// and cut to its last 600 characters; `None` when Them said nothing.
fn them_quote(
    store: &TranscriptStore,
    in_progress: &[InProgressText],
    last_trigger_line_id: u64,
) -> Option<String> {
    let mut them_texts: Vec<&str> = Vec::new();
    for utterance in store.utterances_after(last_trigger_line_id) {
        if utterance.id.speaker == Speaker::Them && !utterance.text.is_empty() {
            them_texts.push(&utterance.text);
        }
    }
    for entry in in_progress {
        if entry.speaker == Speaker::Them && !entry.text.is_empty() {
            them_texts.push(&entry.text);
        }
    }
    if them_texts.is_empty() {
        return None;
    }
    let joined = them_texts.join(" ");
    let char_count = joined.chars().count();
    Some(if char_count > 600 {
        joined.chars().skip(char_count - 600).collect()
    } else {
        joined
    })
}

/// The task sentence: the Them text since the last trigger, quoted, when
/// there is one; the generic question otherwise.
fn task_sentence(
    store: &TranscriptStore,
    in_progress: &[InProgressText],
    last_trigger_line_id: u64,
) -> String {
    match them_quote(store, in_progress, last_trigger_line_id) {
        Some(quoted) => {
            format!("The last thing Them said was: \"{quoted}\". Tell me what to say now.")
        }
        None => String::from("Suggest what I should say next."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clueless_types::Utterance;
    use clueless_types::UtteranceId;

    /// A manual request under the Manual profile with no previous answer.
    fn build(
        store: &TranscriptStore,
        notes: Option<&str>,
        in_progress: &[InProgressText],
        last_trigger_line_id: u64,
    ) -> Vec<PromptMessage> {
        let ask = Ask {
            profile: AssistProfile::Manual,
            origin: Origin::Manual,
            previous_answer: None,
        };
        build_for(store, notes, in_progress, last_trigger_line_id, &ask).messages
    }

    fn utterance(speaker: Speaker, seq: u64, t0_ms: u64, text: &str) -> Utterance {
        Utterance {
            id: UtteranceId { speaker, seq },
            t0_ms,
            t1_ms: t0_ms + 1000,
            text: text.to_string(),
        }
    }

    fn user_message(messages: Vec<PromptMessage>) -> String {
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "user");
        messages[1].content.clone()
    }

    #[test]
    fn transcript_prefix_is_stable_when_a_line_is_committed() {
        // Spec invariant C-prompt-prefix-stable: the user message built after
        // N+1 committed lines starts with the exact bytes of the transcript
        // part built after N lines.
        let mut store = TranscriptStore::new();
        store.push(utterance(Speaker::Them, 0, 1000, "hello there"));
        store.push(utterance(Speaker::Me, 0, 2500, "hi"));
        store.push(utterance(Speaker::Them, 1, 4000, "quick question"));

        let part_after_three = transcript_part(&store);
        let user_after_three = user_message(build(&store, None, &[], 0));
        assert!(user_after_three.starts_with(&part_after_three));

        store.push(utterance(Speaker::Me, 1, 6000, "go ahead"));
        let user_after_four = user_message(build(&store, None, &[], 0));
        assert!(user_after_four.starts_with(&part_after_three));

        // Sanity on the shape: the fourth line sits behind the same bytes.
        assert!(user_after_four.contains("[00:06] Me: go ahead"));
    }

    fn ask(profile: AssistProfile, origin: Origin) -> Ask<'static> {
        Ask {
            profile,
            origin,
            previous_answer: None,
        }
    }

    const SHAPE: &str = "Answer with one sentence that is the direct answer, then at most 3 short lines starting with a dash.";
    const PASS: &str = "If there is nothing useful to add, reply with exactly: PASS";

    #[test]
    fn manual_without_notes_has_bare_rules_and_the_task_then_the_answer_shape() {
        let store = TranscriptStore::new();
        let built = build_for(
            &store,
            None,
            &[],
            0,
            &ask(AssistProfile::Manual, Origin::Manual),
        );
        assert_eq!(
            built.messages[0].content,
            "You are a live meeting assistant. You see the transcript of a meeting in progress. \
             \"Me\" is the user you help. \"Them\" is the other side.\n\
             Write plain text, no markdown. Use at most 60 words. \
             Answer in the language of the conversation."
        );
        assert_eq!(built.messages[0].content, BASE_RULES);
        assert_eq!(
            built.messages[1].content,
            format!("TRANSCRIPT SO FAR:\n\nSuggest what I should say next.\n{SHAPE}")
        );
    }

    #[test]
    fn notes_end_the_system_message_under_about_the_user() {
        let store = TranscriptStore::new();
        let first = build(&store, Some("Daniel, backend engineer"), &[], 0);
        let second = build(&store, Some("Daniel, backend engineer"), &[], 0);

        assert_eq!(first[0], second[0]);
        assert_eq!(
            first[0].content,
            format!("{BASE_RULES}\n\nABOUT THE USER:\nDaniel, backend engineer"),
        );
        assert!(
            first[0]
                .content
                .ends_with("ABOUT THE USER:\nDaniel, backend engineer")
        );
        assert!(!first[0].content.contains("PROFILE:"));
    }

    #[test]
    fn interview_automatic_quotes_them_and_ends_with_the_pass_rule() {
        let mut store = TranscriptStore::new();
        store.push(utterance(
            Speaker::Them,
            0,
            5000,
            "can you ship it tomorrow",
        ));
        let built = build_for(
            &store,
            None,
            &[],
            0,
            &ask(AssistProfile::Interview, Origin::Auto),
        );
        let user = &built.messages[1].content;
        assert!(user.ends_with(&format!(
            "\n\nThe last thing Them said was: \"can you ship it tomorrow\".\n\
             If Them asked a question or raised a point that needs a reply from me, \
             give me the answer to say. {SHAPE}\n{PASS}"
        )));
    }

    #[test]
    fn interview_automatic_without_new_them_text_drops_the_quote_line() {
        let store = TranscriptStore::new();
        let built = build_for(
            &store,
            None,
            &[],
            0,
            &ask(AssistProfile::Interview, Origin::Auto),
        );
        assert_eq!(
            built.messages[1].content,
            format!(
                "TRANSCRIPT SO FAR:\n\nIf Them asked a question or raised a point that needs \
                 a reply from me, give me the answer to say. {SHAPE}\n{PASS}"
            )
        );
    }

    #[test]
    fn interview_manual_is_the_manual_instruction_without_pass() {
        let mut store = TranscriptStore::new();
        store.push(utterance(
            Speaker::Them,
            0,
            5000,
            "can you ship it tomorrow",
        ));
        let interview = build_for(
            &store,
            None,
            &[],
            0,
            &ask(AssistProfile::Interview, Origin::Manual),
        );
        let manual = build_for(
            &store,
            None,
            &[],
            0,
            &ask(AssistProfile::Manual, Origin::Manual),
        );
        assert_eq!(interview.messages, manual.messages);
        let user = &interview.messages[1].content;
        assert!(user.ends_with(&format!(
            "The last thing Them said was: \"can you ship it tomorrow\". \
             Tell me what to say now.\n{SHAPE}"
        )));
        assert!(!user.contains("PASS"));
    }

    #[test]
    fn brainstorm_automatic_ends_with_pass_and_manual_has_none() {
        let mut store = TranscriptStore::new();
        store.push(utterance(Speaker::Me, 0, 5000, "we could cache it"));
        let sentence = "I am talking. Give me up to 3 short lines starting with a dash: \
                        ideas, angles or facts that extend what I am saying and that I have \
                        not said yet.";
        let auto = build_for(
            &store,
            None,
            &[],
            0,
            &ask(AssistProfile::Brainstorm, Origin::Auto),
        );
        assert!(
            auto.messages[1]
                .content
                .ends_with(&format!("\n\n{sentence}\n{PASS}"))
        );
        let manual = build_for(
            &store,
            None,
            &[],
            0,
            &ask(AssistProfile::Brainstorm, Origin::Manual),
        );
        assert!(
            manual.messages[1]
                .content
                .ends_with(&format!("\n\n{sentence}"))
        );
        assert!(!manual.messages[1].content.contains("PASS"));
    }

    #[test]
    fn previous_answer_is_cut_to_600_characters_and_sits_before_the_instruction() {
        let store = TranscriptStore::new();
        let previous = format!("{}{}", "x".repeat(600), "z".repeat(100));
        let ask = Ask {
            profile: AssistProfile::Manual,
            origin: Origin::Manual,
            previous_answer: Some(&previous),
        };
        let user = build_for(&store, None, &[], 0, &ask).messages[1]
            .content
            .clone();
        assert_eq!(
            user,
            format!(
                "TRANSCRIPT SO FAR:\n\n\
                 YOUR PREVIOUS ANSWER (already shown, add only what is new):\n{}\n\n\
                 Suggest what I should say next.\n{SHAPE}",
                "x".repeat(600)
            )
        );
        assert!(!user.contains('z'));
    }

    #[test]
    fn tail_order_is_in_progress_then_previous_answer_then_instruction() {
        let store = TranscriptStore::new();
        let in_progress = [InProgressText {
            speaker: Speaker::Me,
            t0_ms: 3000,
            t1_ms: 4000,
            text: "so I think".to_string(),
        }];
        let ask = Ask {
            profile: AssistProfile::Manual,
            origin: Origin::Manual,
            previous_answer: Some("Yes, tomorrow."),
        };
        let user = build_for(&store, None, &in_progress, 0, &ask).messages[1]
            .content
            .clone();
        assert_eq!(
            user,
            format!(
                "TRANSCRIPT SO FAR:\n\n\
                 IN PROGRESS (may be incomplete):\n[00:03] Me: so I think\n\n\
                 YOUR PREVIOUS ANSWER (already shown, add only what is new):\nYes, tomorrow.\n\n\
                 Suggest what I should say next.\n{SHAPE}"
            )
        );
    }

    #[test]
    fn system_message_and_transcript_part_are_identical_across_profiles() {
        let mut store = TranscriptStore::new();
        store.push(utterance(Speaker::Them, 0, 1000, "hello there"));
        store.push(utterance(Speaker::Me, 0, 2500, "hi"));
        let part = transcript_part(&store);
        let builds: Vec<Built> = AssistProfile::ALL
            .iter()
            .map(|profile| build_for(&store, Some("notes"), &[], 0, &ask(*profile, Origin::Auto)))
            .collect();
        for built in &builds {
            assert_eq!(built.messages[0], builds[0].messages[0]);
            assert!(
                built.messages[1]
                    .content
                    .starts_with(&format!("{part}\n\n"))
            );
        }
    }

    #[test]
    fn build_returns_the_stores_last_line_id() {
        let mut store = TranscriptStore::new();
        store.push(utterance(Speaker::Them, 0, 1000, "one"));
        store.push(utterance(Speaker::Me, 0, 2000, "two"));
        let built = build_for(
            &store,
            None,
            &[],
            0,
            &ask(AssistProfile::Manual, Origin::Manual),
        );
        assert_eq!(built.last_line_id, store.last_line_id());
        assert_ne!(built.last_line_id, 0);
        assert_eq!(
            build_for(
                &TranscriptStore::new(),
                None,
                &[],
                0,
                &ask(AssistProfile::Manual, Origin::Manual)
            )
            .last_line_id,
            0
        );
    }

    #[test]
    fn task_quotes_the_them_final_committed_since_the_last_trigger() {
        let mut store = TranscriptStore::new();
        store.push(utterance(Speaker::Them, 0, 1000, "old news"));
        let trigger = store.last_line_id();
        store.push(utterance(
            Speaker::Them,
            1,
            5000,
            "can you ship it tomorrow",
        ));

        let user = user_message(build(&store, None, &[], trigger));
        assert!(user.ends_with(&format!(
            "The last thing Them said was: \"can you ship it tomorrow\". \
             Tell me what to say now.\n{SHAPE}"
        )));
    }

    #[test]
    fn task_joins_them_finals_and_in_progress_text_since_the_trigger() {
        let mut store = TranscriptStore::new();
        let trigger = store.push(utterance(Speaker::Me, 0, 1000, "my last word"));
        store.push(utterance(Speaker::Them, 0, 2000, "the release is late"));
        store.push(utterance(Speaker::Them, 1, 4000, "and tests fail"));
        let in_progress = [InProgressText {
            speaker: Speaker::Them,
            t0_ms: 6000,
            t1_ms: 7000,
            text: "so we need more time".to_string(),
        }];

        let user = user_message(build(&store, None, &in_progress, trigger));
        assert!(user.ends_with(&format!(
            "The last thing Them said was: \"the release is late and tests fail \
             so we need more time\". Tell me what to say now.\n{SHAPE}"
        )));
    }

    #[test]
    fn task_cuts_the_them_text_to_its_last_600_characters() {
        // 900 characters of Them text since the trigger: the task quotes the
        // last 600, which are all 'b'.
        let mut store = TranscriptStore::new();
        let them_text = format!("{}{}", "a".repeat(300), "b".repeat(600));
        assert_eq!(them_text.chars().count(), 900);
        let trigger = store.push(utterance(Speaker::Me, 0, 1000, "before"));
        store.push(utterance(Speaker::Them, 0, 2000, &them_text));

        let user = user_message(build(&store, None, &[], trigger));
        assert!(user.ends_with(&format!(
            "The last thing Them said was: \"{}\". Tell me what to say now.\n{SHAPE}",
            "b".repeat(600)
        )));
    }

    #[test]
    fn task_asks_the_generic_question_when_them_said_nothing_since_the_trigger() {
        let mut store = TranscriptStore::new();
        store.push(utterance(Speaker::Them, 0, 1000, "a question"));
        let trigger = store.last_line_id();
        store.push(utterance(Speaker::Me, 0, 3000, "my answer"));

        let user = user_message(build(&store, None, &[], trigger));
        assert!(user.ends_with(&format!("Suggest what I should say next.\n{SHAPE}")));
    }

    #[test]
    fn in_progress_block_sits_between_transcript_and_task_and_the_task_quotes_it() {
        let mut store = TranscriptStore::new();
        let trigger = store.push(utterance(Speaker::Them, 0, 1000, "the old line"));
        let in_progress = [InProgressText {
            speaker: Speaker::Them,
            t0_ms: 9000,
            t1_ms: 10_000,
            text: "should we merge it".to_string(),
        }];

        let user = user_message(build(&store, None, &in_progress, trigger));
        let transcript_at = user.find("TRANSCRIPT SO FAR:").unwrap();
        let in_progress_at = user.find("IN PROGRESS (may be incomplete):").unwrap();
        let task_at = user.find("The last thing Them said was:").unwrap();
        assert!(transcript_at < in_progress_at);
        assert!(in_progress_at < task_at);
        assert!(user.contains("[00:09] Them: should we merge it"));
        assert!(user.ends_with(&format!(
            "The last thing Them said was: \"should we merge it\". \
             Tell me what to say now.\n{SHAPE}"
        )));
    }

    #[test]
    fn lines_are_stamped_mm_ss_from_the_start_time() {
        let mut store = TranscriptStore::new();
        store.push(utterance(Speaker::Them, 0, 65_000, "text"));

        let user = user_message(build(&store, None, &[], 0));
        assert!(user.contains("[01:05] Them: text"));
    }

    #[test]
    fn empty_store_user_message_is_header_and_task_only() {
        let store = TranscriptStore::new();
        let user = user_message(build(&store, None, &[], 0));
        assert_eq!(
            user,
            format!("TRANSCRIPT SO FAR:\n\nSuggest what I should say next.\n{SHAPE}")
        );
        assert!(!user.contains("\n\n\n"));
        assert!(!user.ends_with('\n'));
    }
}
