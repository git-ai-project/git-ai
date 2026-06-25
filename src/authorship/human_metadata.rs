use crate::authorship::authorship_log::HumanRecord;
use crate::authorship::authorship_log_serialization::{AuthorshipLog, generate_human_short_hash};
use crate::git::repository::Repository;

pub(crate) fn fill_missing_current_human_metadata(repo: &Repository, log: &mut AuthorshipLog) {
    let author = repo.effective_author_identity().formatted_or_unknown();
    fill_missing_human_metadata_for_author(log, &author);
}

fn fill_missing_human_metadata_for_author(log: &mut AuthorshipLog, author: &str) {
    let human_id = generate_human_short_hash(author);
    if log.metadata.humans.contains_key(&human_id) {
        return;
    }

    let references_human = log
        .attestations
        .iter()
        .flat_map(|attestation| attestation.entries.iter())
        .any(|entry| entry.hash.split("::").next().unwrap_or(&entry.hash) == human_id);

    if references_human {
        log.metadata.humans.insert(
            human_id,
            HumanRecord {
                author: author.to_string(),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authorship::authorship_log::LineRange;
    use crate::authorship::authorship_log_serialization::{AttestationEntry, FileAttestation};

    #[test]
    fn fills_missing_metadata_for_referenced_current_human() {
        let author = "Test User <test@example.com>";
        let human_id = generate_human_short_hash(author);
        let mut log = AuthorshipLog::new();
        let mut file = FileAttestation::new("version.py".to_string());
        file.add_entry(AttestationEntry::new(
            human_id.clone(),
            vec![LineRange::Single(2)],
        ));
        log.attestations.push(file);

        fill_missing_human_metadata_for_author(&mut log, author);

        assert_eq!(log.metadata.humans[&human_id].author, author);
    }

    #[test]
    fn does_not_fill_unrelated_human_hash() {
        let mut log = AuthorshipLog::new();
        let mut file = FileAttestation::new("version.py".to_string());
        file.add_entry(AttestationEntry::new(
            "h_00000000000000".to_string(),
            vec![LineRange::Single(2)],
        ));
        log.attestations.push(file);

        fill_missing_human_metadata_for_author(&mut log, "Test User <test@example.com>");

        assert!(log.metadata.humans.is_empty());
    }
}
