//! The assist profiles: cycle order, names and the config keys.

use clueless_types::profile::AssistProfile;

#[test]
fn next_cycles_manual_interview_brainstorm_and_back() {
    assert_eq!(AssistProfile::Manual.next(), AssistProfile::Interview);
    assert_eq!(AssistProfile::Interview.next(), AssistProfile::Brainstorm);
    assert_eq!(AssistProfile::Brainstorm.next(), AssistProfile::Manual);
}

#[test]
fn a_lowercase_key_parses_and_a_capitalised_one_does_not() {
    assert_eq!(
        "interview".parse::<AssistProfile>(),
        Ok(AssistProfile::Interview)
    );
    assert!("Interview".parse::<AssistProfile>().is_err());
}

#[test]
fn names_are_capitalised_and_keys_are_lowercase() {
    let names: Vec<_> = AssistProfile::ALL.iter().map(|p| p.name()).collect();
    assert_eq!(names, ["Manual", "Interview", "Brainstorm"]);
    let keys: Vec<_> = AssistProfile::ALL.iter().map(|p| p.key()).collect();
    assert_eq!(keys, ["manual", "interview", "brainstorm"]);
}
