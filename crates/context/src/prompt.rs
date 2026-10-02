//! Prompt building: one system and one user message whose transcript part is
//! append-only, so the server's prefix cache keeps hitting (C-prompt-prefix-stable).

use clueless_types::Speaker;

use crate::store::TranscriptStore;

/// The fixed rules of the system message, including the answer shape.
pub const RULES: &str = concat!(
    "You are a live meeting assistant. You see the transcript of a meeting in ",
    "progress and tell the user what to say next.\n",
    "Answer with one sentence that is the direct answer, then at most 3 short ",
    "lines starting with a dash.\n",
    "Use at most 60 words in total. Write plain text, no markdown. ",
    "Answer in the language of the conversation.",
);

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

/// Build the two chat messages: the system message (rules, plus the profile
/// when one is configured) and the user message (transcript part, a blank
/// line, then the tail of in-progress text and the task sentence).
///
/// `last_trigger_line_id` is the store's `last_line_id` at the previous
/// trigger (0 before the first one) and decides which Them lines the task
/// quotes.
pub fn build(
    store: &TranscriptStore,
    profile: Option<&str>,
    in_progress: &[InProgressText],
    last_trigger_line_id: u64,
) -> Vec<PromptMessage> {
    let system = match profile {
        Some(text) => format!("{RULES}\n\nPROFILE:\n{text}"),
        None => RULES.to_string(),
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
    tail_parts.push(task_sentence(store, in_progress, last_trigger_line_id));

    let user = format!("{}\n\n{}", transcript_part(store), tail_parts.join("\n\n"));

    vec![
        PromptMessage {
            role: "system".to_string(),
            content: system,
        },
        PromptMessage {
            role: "user".to_string(),
            content: user,
        },
    ]
}

/// The task sentence: the Them text since the last trigger, quoted, when
/// there is one; the generic question otherwise.
fn task_sentence(
    store: &TranscriptStore,
    in_progress: &[InProgressText],
    last_trigger_line_id: u64,
) -> String {
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
        return String::from("Suggest what I should say next.");
    }
    let joined = them_texts.join(" ");
    let char_count = joined.chars().count();
    let quoted: String = if char_count > 600 {
        joined.chars().skip(char_count - 600).collect()
    } else {
        joined
    };
    format!("The last thing Them said was: \"{quoted}\". Tell me what to say now.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use clueless_types::Utterance;
    use clueless_types::UtteranceId;

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

    #[test]
    fn profile_goes_into_the_system_message_and_is_stable() {
        let store = TranscriptStore::new();
        let first = build(&store, Some("Daniel, backend engineer"), &[], 0);
        let second = build(&store, Some("Daniel, backend engineer"), &[], 0);

        assert_eq!(first[0], second[0]);
        assert_eq!(
            first[0].content,
            format!("{RULES}\n\nPROFILE:\nDaniel, backend engineer"),
        );
        assert!(first[0].content.ends_with("Daniel, backend engineer"));
    }

    #[test]
    fn system_message_without_profile_is_just_the_rules() {
        let store = TranscriptStore::new();
        let messages = build(&store, None, &[], 0);
        assert_eq!(messages[0].content, RULES);
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
        assert!(user.ends_with(
            "The last thing Them said was: \"can you ship it tomorrow\". \
             Tell me what to say now."
        ));
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
        assert!(user.ends_with(
            "The last thing Them said was: \"the release is late and tests fail \
             so we need more time\". Tell me what to say now."
        ));
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
            "The last thing Them said was: \"{}\". Tell me what to say now.",
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
        assert!(user.ends_with("Suggest what I should say next."));
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
        assert!(user.ends_with(
            "The last thing Them said was: \"should we merge it\". \
             Tell me what to say now."
        ));
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
            "TRANSCRIPT SO FAR:\n\nSuggest what I should say next."
        );
        assert!(!user.contains("\n\n\n"));
        assert!(!user.ends_with('\n'));
    }
}
