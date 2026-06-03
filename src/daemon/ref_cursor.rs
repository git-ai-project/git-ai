use crate::daemon::analyzers::command_args;
use crate::daemon::domain::{Confidence, FamilyKey, FamilyState, NormalizedCommand, RefChange};
use crate::error::GitAiError;
use crate::git::find_repository_in_path;
use crate::git::repo_state::{git_dir_for_worktree, is_valid_git_oid};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct RefCursor {
    family: FamilyKey,
    offsets: HashMap<String, u64>,
    stash_stack: Vec<String>,
}

#[derive(Debug, Clone)]
struct CursorEntry {
    key: String,
    reference: String,
    old: String,
    new: String,
    message: String,
    end_offset: u64,
}

#[derive(Debug, Clone)]
struct UpdateRefSpec {
    reference: String,
    new_oid: String,
    old_oid: Option<String>,
}

#[derive(Debug, Clone)]
enum BranchCommandSpec {
    CreateOrReset {
        reference: String,
    },
    Delete {
        references: Vec<String>,
    },
    Rename {
        old_reference: Option<String>,
        new_reference: String,
    },
    Copy {
        old_reference: Option<String>,
        new_reference: String,
    },
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BranchLifecycleKind {
    Rename,
    Copy,
}

#[derive(Debug, Clone)]
struct BranchLifecycleRecord {
    old_reference: String,
    oid: String,
}

#[derive(Debug, Clone)]
struct ReflogRecord {
    old: String,
    new: String,
    message: String,
    end_offset: u64,
}

impl RefCursor {
    pub fn new(family: FamilyKey) -> Self {
        Self {
            family,
            offsets: HashMap::new(),
            stash_stack: Vec::new(),
        }
    }

    pub fn enrich_command(
        &mut self,
        cmd: &mut NormalizedCommand,
        state: &FamilyState,
    ) -> Result<(), GitAiError> {
        cmd.ref_changes.clear();

        if cmd.exit_code != 0 && !command_can_move_refs_on_nonzero(cmd.primary_command.as_deref()) {
            return Ok(());
        }

        let Some(primary) = cmd.primary_command.as_deref() else {
            return Ok(());
        };
        if !command_uses_ref_cursor(primary) {
            return Ok(());
        }

        match primary {
            "commit" => self.enrich_commit(cmd, state),
            "revert" => self.consume_head_transition_for_command(
                cmd,
                state,
                &["revert:"],
                ExpectedTransition::from_state_and_working_logs(cmd, state),
            ),
            "reset" => self.consume_head_transition_for_command(
                cmd,
                state,
                &["reset:"],
                ExpectedTransition::from_state_and_working_logs(cmd, state),
            ),
            "checkout" => {
                if checkout_is_path_checkout(cmd) {
                    Ok(())
                } else {
                    self.consume_head_transition_for_command(
                        cmd,
                        state,
                        &["checkout:"],
                        ExpectedTransition::from_state_and_working_logs(cmd, state),
                    )
                }
            }
            "switch" => self.consume_head_transition_for_command(
                cmd,
                state,
                &["checkout:", "switch:"],
                ExpectedTransition::from_state_and_working_logs(cmd, state),
            ),
            "merge" => self.consume_head_transition_for_command(
                cmd,
                state,
                &["merge"],
                ExpectedTransition::from_state_and_working_logs(cmd, state),
            ),
            "cherry-pick" => {
                let args = command_args(cmd);
                if args.iter().any(|arg| arg == "--no-commit" || arg == "-n") {
                    Ok(())
                } else {
                    self.consume_head_span_for_command(
                        cmd,
                        state,
                        &["cherry-pick:", "commit:", "commit (cherry-pick):"],
                        ExpectedTransition::from_state_and_working_logs(cmd, state),
                    )
                }
            }
            "rebase" => self.consume_rebase_transition(cmd, state),
            "pull" => self.consume_head_span_for_command(
                cmd,
                state,
                &["pull", "merge", "rebase", "checkout:", "commit:"],
                ExpectedTransition::from_state_and_working_logs(cmd, state),
            ),
            "branch" => self.enrich_branch(cmd, state),
            "stash" => self.enrich_stash(cmd, state),
            "update-ref" => self.enrich_update_ref(cmd, state),
            _ => Ok(()),
        }?;

        if !cmd.ref_changes.is_empty() {
            cmd.confidence = Confidence::High;
        }
        Ok(())
    }

    fn enrich_commit(
        &mut self,
        cmd: &mut NormalizedCommand,
        state: &FamilyState,
    ) -> Result<(), GitAiError> {
        let args = command_args(cmd);
        let amend = args.iter().any(|arg| arg == "--amend");
        let prefixes = if amend {
            &["commit (amend):", "commit:"] as &[&str]
        } else {
            &["commit", "commit (initial):"]
        };
        let expected = ExpectedTransition::from_state_and_working_logs(cmd, state)
            .with_reflog_messages(commit_reflog_messages(&args, amend));
        self.consume_head_transition_for_command(cmd, state, prefixes, expected)
    }

    fn enrich_branch(
        &mut self,
        cmd: &mut NormalizedCommand,
        state: &FamilyState,
    ) -> Result<(), GitAiError> {
        let args = command_args(cmd);
        let spec = parse_branch_command_spec(&args);
        let mut changes = Vec::new();

        match spec {
            BranchCommandSpec::CreateOrReset { reference } => {
                if let Some(entry) = self.find_common_ref_entry(
                    &reference,
                    ExpectedTransition::default(),
                    &["branch:"],
                )? {
                    self.consume_entry(&entry);
                    changes.push(entry_to_ref_change(&entry));
                }
            }
            BranchCommandSpec::Delete { references } => {
                let zero = zero_oid();
                for reference in references {
                    self.offsets.remove(&common_key(&reference));
                    if let Some(old) = state
                        .refs
                        .get(&reference)
                        .filter(|oid| valid_non_zero_oid(oid))
                    {
                        changes.push(RefChange {
                            reference,
                            old: old.clone(),
                            new: zero.clone(),
                        });
                    }
                }
            }
            BranchCommandSpec::Rename {
                old_reference,
                new_reference,
            } => {
                self.enrich_branch_relocation(
                    state,
                    BranchLifecycleKind::Rename,
                    old_reference,
                    new_reference,
                    &mut changes,
                )?;
            }
            BranchCommandSpec::Copy {
                old_reference,
                new_reference,
            } => {
                self.enrich_branch_relocation(
                    state,
                    BranchLifecycleKind::Copy,
                    old_reference,
                    new_reference,
                    &mut changes,
                )?;
            }
            BranchCommandSpec::None => {}
        }

        dedup_ref_changes(&mut changes);
        cmd.ref_changes = changes;
        Ok(())
    }

    fn enrich_branch_relocation(
        &mut self,
        state: &FamilyState,
        kind: BranchLifecycleKind,
        old_reference: Option<String>,
        new_reference: String,
        changes: &mut Vec<RefChange>,
    ) -> Result<(), GitAiError> {
        let lifecycle = self.latest_branch_lifecycle_record(&new_reference, kind)?;
        let source_reference = old_reference.or_else(|| {
            lifecycle
                .as_ref()
                .map(|record| record.old_reference.clone())
        });
        let source_oid = source_reference
            .as_ref()
            .and_then(|reference| state.refs.get(reference).cloned())
            .or_else(|| lifecycle.as_ref().map(|record| record.oid.clone()));
        let Some(source_oid) = source_oid.filter(|oid| valid_non_zero_oid(oid)) else {
            self.advance_common_ref_cursor_to_log_end(&new_reference)?;
            return Ok(());
        };

        if kind == BranchLifecycleKind::Rename
            && let Some(source_reference) = source_reference.as_ref()
            && source_reference != &new_reference
        {
            self.offsets.remove(&common_key(source_reference));
            changes.push(RefChange {
                reference: source_reference.clone(),
                old: source_oid.clone(),
                new: zero_oid(),
            });
        }

        let new_old = state
            .refs
            .get(&new_reference)
            .filter(|oid| valid_non_zero_oid(oid))
            .cloned()
            .unwrap_or_else(zero_oid);
        if new_old != source_oid {
            changes.push(RefChange {
                reference: new_reference.clone(),
                old: new_old,
                new: source_oid,
            });
        }
        self.advance_common_ref_cursor_to_log_end(&new_reference)?;
        Ok(())
    }

    fn enrich_update_ref(
        &mut self,
        cmd: &mut NormalizedCommand,
        _state: &FamilyState,
    ) -> Result<(), GitAiError> {
        let args = command_args(cmd);
        let spec = parse_update_ref_spec(&args)?;
        let Some(spec) = spec else {
            return Ok(());
        };

        let mut changes = Vec::new();
        if spec.reference == "HEAD" {
            if let Some(entry) = self.find_head_entry(
                cmd.worktree.as_deref(),
                &[],
                ExpectedTransition {
                    old_oids: spec.old_oid.iter().cloned().collect(),
                    new_oid: Some(spec.new_oid.clone()),
                    messages: HashSet::new(),
                },
            )? {
                self.consume_entry(&entry);
                changes.push(entry_to_ref_change(&entry));
                self.consume_common_refs_matching_transition(&entry.old, &entry.new, &mut changes)?;
            }
        } else if let Some(entry) = self.find_common_ref_entry(
            &spec.reference,
            ExpectedTransition {
                old_oids: spec.old_oid.iter().cloned().collect(),
                new_oid: Some(spec.new_oid.clone()),
                messages: HashSet::new(),
            },
            &[],
        )? {
            self.consume_entry(&entry);
            let old = entry.old.clone();
            let new = entry.new.clone();
            changes.push(entry_to_ref_change(&entry));
            if let Some(head) = self.find_head_entry(
                cmd.worktree.as_deref(),
                &[],
                ExpectedTransition {
                    old_oids: [old.clone()].into_iter().collect(),
                    new_oid: Some(new.clone()),
                    messages: HashSet::new(),
                },
            )? {
                self.consume_entry(&head);
                changes.push(entry_to_ref_change(&head));
            }
        }

        dedup_ref_changes(&mut changes);
        cmd.ref_changes = changes;
        Ok(())
    }

    fn enrich_stash(
        &mut self,
        cmd: &mut NormalizedCommand,
        state: &FamilyState,
    ) -> Result<(), GitAiError> {
        let args = command_args(cmd);
        let stash_args = stash_command_args(&args);
        let kind = stash_args.first().map(String::as_str).unwrap_or("push");

        if matches!(kind, "apply" | "pop" | "drop" | "branch") {
            let target = if kind == "branch" {
                stash_args.get(2)
            } else {
                stash_args.get(1)
            };
            cmd.stash_target_oid = self.resolve_stash_target_at_cursor(target)?;
        }

        if matches!(kind, "push" | "save") {
            let expected = ExpectedTransition::default();
            if let Some(entry) = self.find_common_ref_entry("refs/stash", expected, &[])? {
                self.consume_entry(&entry);
                self.apply_stash_ref_entry(kind, &entry);
                cmd.ref_changes.push(entry_to_ref_change(&entry));
            }
        } else if matches!(kind, "pop" | "drop") {
            self.consume_destructive_stash_operation(stash_args.get(1), cmd)?;
        }

        if matches!(kind, "apply" | "pop" | "branch")
            && (kind == "branch" || !state.refs.contains_key("HEAD"))
        {
            let expected = if kind == "branch" {
                ExpectedTransition::from_state_and_working_logs(cmd, state)
            } else {
                ExpectedTransition::default()
            };
            if let Some(head) = self.find_head_entry(cmd.worktree.as_deref(), &[], expected)?
                && message_matches(&head.message, &["reset:", "checkout:"])
            {
                self.consume_entry(&head);
                cmd.ref_changes.push(entry_to_ref_change(&head));
            }
        }

        Ok(())
    }

    fn consume_destructive_stash_operation(
        &mut self,
        target: Option<&String>,
        cmd: &mut NormalizedCommand,
    ) -> Result<(), GitAiError> {
        let key = common_key("refs/stash");
        let old_cursor = self.offsets.get(&key).copied();
        let log_len_after = self.common_ref_log_len("refs/stash")?;
        let log_was_rewritten = match (old_cursor, log_len_after) {
            (Some(cursor), Some(len)) => len < cursor,
            (Some(_), None) => true,
            _ => false,
        };

        if !log_was_rewritten {
            return Ok(());
        }

        let target_oid = cmd
            .stash_target_oid
            .clone()
            .or_else(|| self.resolve_stash_target_at_cursor(target).ok().flatten());
        let Some(target_oid) = target_oid else {
            self.advance_common_ref_cursor_to_log_end("refs/stash")?;
            return Ok(());
        };

        let target_index = stash_target_index(target);
        let old_top = self.stash_stack.first().cloned();
        self.remove_stash_from_stack(target_index, &target_oid);
        let new_top = self.stash_stack.first().cloned().unwrap_or_else(zero_oid);

        if old_top.as_deref() == Some(target_oid.as_str()) {
            cmd.ref_changes.push(RefChange {
                reference: "refs/stash".to_string(),
                old: target_oid.clone(),
                new: new_top,
            });
        }
        if cmd.stash_target_oid.is_none() {
            cmd.stash_target_oid = Some(target_oid);
        }

        self.advance_common_ref_cursor_to_log_end("refs/stash")?;
        Ok(())
    }

    fn consume_rebase_transition(
        &mut self,
        cmd: &mut NormalizedCommand,
        state: &FamilyState,
    ) -> Result<(), GitAiError> {
        let expected = ExpectedTransition::from_state_and_working_logs(cmd, state);
        let Some(first) =
            self.find_head_entry(cmd.worktree.as_deref(), &["rebase", "checkout:"], expected)?
        else {
            return Ok(());
        };

        let mut changes = vec![entry_to_ref_change(&first)];
        let old = first.old.clone();
        let mut new = first.new.clone();
        self.consume_entry(&first);

        while let Some(next) = self.find_head_entry(
            cmd.worktree.as_deref(),
            &["rebase"],
            ExpectedTransition::default(),
        )? {
            new = next.new.clone();
            self.consume_entry(&next);
            changes.push(entry_to_ref_change(&next));
        }

        self.consume_common_refs_matching_transition(&old, &new, &mut changes)?;
        self.consume_common_refs_with_new(&new, &["rebase"], &mut changes)?;
        dedup_ref_changes(&mut changes);
        cmd.ref_changes = changes;
        Ok(())
    }

    fn consume_head_transition_for_command(
        &mut self,
        cmd: &mut NormalizedCommand,
        _state: &FamilyState,
        message_prefixes: &[&str],
        expected: ExpectedTransition,
    ) -> Result<(), GitAiError> {
        let Some(entry) =
            self.find_head_entry(cmd.worktree.as_deref(), message_prefixes, expected)?
        else {
            return Ok(());
        };

        self.consume_entry(&entry);
        let old = entry.old.clone();
        let new = entry.new.clone();
        let mut changes = vec![entry_to_ref_change(&entry)];
        self.consume_common_refs_matching_transition(&old, &new, &mut changes)?;
        self.consume_aux_refs_for_head_move(&old, &new, &mut changes)?;
        dedup_ref_changes(&mut changes);
        cmd.ref_changes = changes;
        Ok(())
    }

    fn consume_head_span_for_command(
        &mut self,
        cmd: &mut NormalizedCommand,
        _state: &FamilyState,
        message_prefixes: &[&str],
        expected: ExpectedTransition,
    ) -> Result<(), GitAiError> {
        let Some(first) =
            self.find_head_entry(cmd.worktree.as_deref(), message_prefixes, expected)?
        else {
            return Ok(());
        };

        let old = first.old.clone();
        let mut new = first.new.clone();
        let mut changes = vec![entry_to_ref_change(&first)];
        self.consume_entry(&first);

        while let Some(next) = self.find_head_entry(
            cmd.worktree.as_deref(),
            message_prefixes,
            ExpectedTransition::default(),
        )? {
            new = next.new.clone();
            self.consume_entry(&next);
            changes.push(entry_to_ref_change(&next));
        }

        self.consume_common_refs_matching_transition(&old, &new, &mut changes)?;
        self.consume_aux_refs_for_head_move(&old, &new, &mut changes)?;
        dedup_ref_changes(&mut changes);
        cmd.ref_changes = changes;
        Ok(())
    }

    fn find_head_entry(
        &self,
        worktree: Option<&Path>,
        message_prefixes: &[&str],
        expected: ExpectedTransition,
    ) -> Result<Option<CursorEntry>, GitAiError> {
        let Some(worktree) = worktree else {
            return Ok(None);
        };
        let Some(git_dir) = git_dir_for_worktree(worktree) else {
            return Ok(None);
        };
        let path = git_dir.join("logs").join("HEAD");
        self.find_entry_in_log(
            head_key(&git_dir),
            &path,
            "HEAD",
            expected,
            message_prefixes,
        )
    }

    fn find_common_ref_entry(
        &self,
        reference: &str,
        expected: ExpectedTransition,
        message_prefixes: &[&str],
    ) -> Result<Option<CursorEntry>, GitAiError> {
        let path = self.common_dir().join("logs").join(reference);
        self.find_entry_in_log(
            common_key(reference),
            &path,
            reference,
            expected,
            message_prefixes,
        )
    }

    fn find_entry_in_log(
        &self,
        key: String,
        path: &Path,
        reference: &str,
        expected: ExpectedTransition,
        message_prefixes: &[&str],
    ) -> Result<Option<CursorEntry>, GitAiError> {
        let entries = read_reflog_entries(
            key.clone(),
            path,
            reference,
            self.offsets.get(&key).copied(),
        )?;
        Ok(entries.into_iter().find(|entry| {
            expected.matches(entry) && message_matches(&entry.message, message_prefixes)
        }))
    }

    fn consume_common_refs_matching_transition(
        &mut self,
        old: &str,
        new: &str,
        out: &mut Vec<RefChange>,
    ) -> Result<(), GitAiError> {
        let refs = self.discover_common_refs()?;
        for reference in refs {
            if reference == "HEAD" || reference == "ORIG_HEAD" || reference == "refs/stash" {
                continue;
            }
            let expected = ExpectedTransition {
                old_oids: [old.to_string()].into_iter().collect(),
                new_oid: Some(new.to_string()),
                messages: HashSet::new(),
            };
            if let Some(entry) = self.find_common_ref_entry(&reference, expected, &[])? {
                self.consume_entry(&entry);
                out.push(entry_to_ref_change(&entry));
            }
        }
        Ok(())
    }

    fn consume_common_refs_with_new(
        &mut self,
        new: &str,
        message_prefixes: &[&str],
        out: &mut Vec<RefChange>,
    ) -> Result<(), GitAiError> {
        let refs = self.discover_common_refs()?;
        for reference in refs {
            if reference == "HEAD" || reference == "ORIG_HEAD" || reference == "refs/stash" {
                continue;
            }
            let expected = ExpectedTransition {
                old_oids: HashSet::new(),
                new_oid: Some(new.to_string()),
                messages: HashSet::new(),
            };
            if let Some(entry) =
                self.find_common_ref_entry(&reference, expected, message_prefixes)?
            {
                self.consume_entry(&entry);
                out.push(entry_to_ref_change(&entry));
            }
        }
        Ok(())
    }

    fn consume_aux_refs_for_head_move(
        &mut self,
        old: &str,
        new: &str,
        out: &mut Vec<RefChange>,
    ) -> Result<(), GitAiError> {
        let reference = "ORIG_HEAD";
        let expected = ExpectedTransition {
            old_oids: HashSet::new(),
            new_oid: None,
            messages: HashSet::new(),
        };
        if let Some(entry) = self.find_common_ref_entry(reference, expected, &[])?
            && (entry.new == old || entry.new == new || entry.old == old)
        {
            self.consume_entry(&entry);
            out.push(entry_to_ref_change(&entry));
        }
        Ok(())
    }

    fn resolve_stash_target_at_cursor(
        &self,
        target: Option<&String>,
    ) -> Result<Option<String>, GitAiError> {
        let target = target.map(String::as_str).unwrap_or("stash@{0}");
        if is_valid_git_oid(target) {
            return Ok(Some(target.to_string()));
        }
        if matches!(target, "stash" | "refs/stash") {
            return self.resolve_stash_target_at_cursor(Some(&"stash@{0}".to_string()));
        }
        let Some(index) = target
            .strip_prefix("stash@{")
            .and_then(|value| value.strip_suffix('}'))
            .and_then(|value| value.parse::<usize>().ok())
        else {
            return Ok(None);
        };
        if let Some(oid) = self.stash_stack.get(index) {
            return Ok(Some(oid.clone()));
        }
        let path = self.common_dir().join("logs").join("refs/stash");
        let key = common_key("refs/stash");
        let entries = read_reflog_entries(key.clone(), &path, "refs/stash", Some(0))?;
        let cursor = self.offsets.get(&key).copied().unwrap_or(u64::MAX);
        let mut stack = entries
            .into_iter()
            .filter(|entry| entry.end_offset <= cursor)
            .filter(|entry| valid_non_zero_oid(&entry.new))
            .map(|entry| entry.new)
            .collect::<Vec<_>>();
        stack.reverse();
        Ok(stack.get(index).cloned())
    }

    fn apply_stash_ref_entry(&mut self, kind: &str, entry: &CursorEntry) {
        match kind {
            "push" | "save" => {
                if valid_non_zero_oid(&entry.new)
                    && !self.stash_stack.iter().any(|oid| oid == &entry.new)
                {
                    self.stash_stack.insert(0, entry.new.clone());
                }
            }
            "pop" | "drop" | "branch" => {
                if let Some(position) = self.stash_stack.iter().position(|oid| oid == &entry.old) {
                    self.stash_stack.remove(position);
                }
                if valid_non_zero_oid(&entry.new)
                    && !self.stash_stack.iter().any(|oid| oid == &entry.new)
                {
                    self.stash_stack.insert(0, entry.new.clone());
                }
            }
            _ => {}
        }
    }

    fn discover_common_refs(&self) -> Result<Vec<String>, GitAiError> {
        let logs = self.common_dir().join("logs");
        let mut refs = Vec::new();
        discover_reflog_refs(&logs, &logs, &mut refs)?;
        refs.sort();
        refs.dedup();
        Ok(refs)
    }

    fn consume_entry(&mut self, entry: &CursorEntry) {
        self.offsets.insert(entry.key.clone(), entry.end_offset);
    }

    fn latest_branch_lifecycle_record(
        &self,
        reference: &str,
        kind: BranchLifecycleKind,
    ) -> Result<Option<BranchLifecycleRecord>, GitAiError> {
        let path = self.common_dir().join("logs").join(reference);
        let records = read_reflog_records(&path, Some(0))?;
        Ok(records.into_iter().rev().find_map(|record| {
            parse_branch_lifecycle_message(kind, &record.message).and_then(
                |(old_reference, new_reference)| {
                    if new_reference != reference {
                        return None;
                    }
                    Some(BranchLifecycleRecord {
                        old_reference,
                        oid: record.new,
                    })
                },
            )
        }))
    }

    fn advance_common_ref_cursor_to_log_end(&mut self, reference: &str) -> Result<(), GitAiError> {
        let key = common_key(reference);
        let path = self.common_dir().join("logs").join(reference);
        match fs::metadata(path) {
            Ok(metadata) => {
                self.offsets.insert(key, metadata.len());
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.offsets.remove(&key);
                Ok(())
            }
            Err(error) => Err(GitAiError::IoError(error)),
        }
    }

    fn common_ref_log_len(&self, reference: &str) -> Result<Option<u64>, GitAiError> {
        let path = self.common_dir().join("logs").join(reference);
        match fs::metadata(path) {
            Ok(metadata) => Ok(Some(metadata.len())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(GitAiError::IoError(error)),
        }
    }

    fn remove_stash_from_stack(&mut self, target_index: Option<usize>, target_oid: &str) {
        if let Some(index) = target_index
            && self
                .stash_stack
                .get(index)
                .is_some_and(|oid| oid == target_oid)
        {
            self.stash_stack.remove(index);
            return;
        }
        if let Some(position) = self.stash_stack.iter().position(|oid| oid == target_oid) {
            self.stash_stack.remove(position);
        }
    }

    fn common_dir(&self) -> PathBuf {
        PathBuf::from(&self.family.0)
    }
}

#[derive(Debug, Clone, Default)]
struct ExpectedTransition {
    old_oids: HashSet<String>,
    new_oid: Option<String>,
    messages: HashSet<String>,
}

impl ExpectedTransition {
    fn with_reflog_messages(mut self, messages: HashSet<String>) -> Self {
        self.messages = messages;
        self
    }

    fn from_state_and_working_logs(cmd: &NormalizedCommand, state: &FamilyState) -> Self {
        let mut old_oids = HashSet::new();
        if let Some(head) = state
            .refs
            .get("HEAD")
            .filter(|head| valid_non_zero_oid(head))
        {
            old_oids.insert(head.clone());
        }
        for (reference, oid) in &state.refs {
            if reference.starts_with("refs/heads/") && valid_non_zero_oid(oid) {
                old_oids.insert(oid.clone());
            }
        }
        if let Some(worktree) = cmd.worktree.as_ref() {
            old_oids.extend(working_log_base_oids(worktree));
        }
        Self {
            old_oids,
            new_oid: None,
            messages: HashSet::new(),
        }
    }

    fn matches(&self, entry: &CursorEntry) -> bool {
        if !valid_ref_transition(&entry.old, &entry.new) {
            return false;
        }
        if !self.messages.is_empty() && !self.messages.contains(&entry.message) {
            return false;
        }
        if !self.old_oids.is_empty() && !self.old_oids.contains(&entry.old) {
            return false;
        }
        if let Some(new_oid) = self.new_oid.as_ref()
            && &entry.new != new_oid
        {
            return false;
        }
        true
    }
}

fn commit_reflog_messages(args: &[String], amend: bool) -> HashSet<String> {
    let Some(subject) = commit_subject_from_args(args) else {
        return HashSet::new();
    };
    let modes = if amend {
        ["commit (amend):", "commit:"].as_slice()
    } else {
        [
            "commit:",
            "commit (initial):",
            "commit (merge):",
            "commit (cherry-pick):",
            "commit (revert):",
        ]
        .as_slice()
    };
    modes
        .iter()
        .map(|mode| format!("{} {}", mode, subject))
        .collect()
}

fn commit_subject_from_args(args: &[String]) -> Option<String> {
    let mut idx = if args.first().is_some_and(|arg| arg == "commit") {
        1
    } else {
        0
    };
    while idx < args.len() {
        let arg = &args[idx];
        match arg.as_str() {
            "-m" | "--message" => {
                return args.get(idx + 1).and_then(|value| commit_subject(value));
            }
            value if value.starts_with("--message=") => {
                return value.strip_prefix("--message=").and_then(commit_subject);
            }
            value if value.starts_with("-m") && value.len() > 2 => {
                return commit_subject(&value[2..]);
            }
            "--" => return None,
            _ => idx += 1,
        }
    }
    None
}

fn commit_subject(message: &str) -> Option<String> {
    message
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.to_string())
}

fn read_reflog_entries(
    key: String,
    path: &Path,
    reference: &str,
    start_offset: Option<u64>,
) -> Result<Vec<CursorEntry>, GitAiError> {
    let records = read_reflog_records(path, start_offset)?;
    Ok(records
        .into_iter()
        .filter(|record| record.old != record.new)
        .map(|record| CursorEntry {
            key: key.clone(),
            reference: reference.to_string(),
            old: record.old,
            new: record.new,
            message: record.message,
            end_offset: record.end_offset,
        })
        .collect())
}

fn read_reflog_records(
    path: &Path,
    start_offset: Option<u64>,
) -> Result<Vec<ReflogRecord>, GitAiError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(GitAiError::IoError(error)),
    };
    let byte_len = bytes.len() as u64;
    let start = match start_offset {
        Some(offset) if offset > byte_len => 0,
        Some(offset) => offset,
        None => 0,
    };

    let mut entries = Vec::new();
    let mut offset = 0u64;
    for raw_line in bytes.split_inclusive(|byte| *byte == b'\n') {
        let line_start = offset;
        offset = offset.saturating_add(raw_line.len() as u64);
        if offset <= start {
            continue;
        }
        let line = String::from_utf8_lossy(raw_line);
        let line = line.trim_end_matches(['\r', '\n']);
        let Some(entry) = parse_reflog_line(line, offset) else {
            continue;
        };
        if entry.end_offset > line_start {
            entries.push(entry);
        }
    }
    Ok(entries)
}

fn parse_reflog_line(line: &str, end_offset: u64) -> Option<ReflogRecord> {
    let (head, message) = line.split_once('\t').unwrap_or((line, ""));
    let mut parts = head.split_whitespace();
    let old = parts.next()?.trim();
    let new = parts.next()?.trim();
    if !is_valid_git_oid(old) || !is_valid_git_oid(new) {
        return None;
    }
    Some(ReflogRecord {
        old: old.to_string(),
        new: new.to_string(),
        message: message.to_string(),
        end_offset,
    })
}

fn discover_reflog_refs(
    root: &Path,
    current: &Path,
    out: &mut Vec<String>,
) -> Result<(), GitAiError> {
    if !current.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            discover_reflog_refs(root, &path, out)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        let reference = relative.to_string_lossy().replace('\\', "/");
        if reference == "HEAD" || reference == "ORIG_HEAD" || reference.starts_with("refs/") {
            out.push(reference);
        }
    }
    Ok(())
}

fn parse_update_ref_spec(args: &[String]) -> Result<Option<UpdateRefSpec>, GitAiError> {
    let mut positionals = Vec::new();
    let mut delete = false;
    let mut idx = 0usize;
    while idx < args.len() {
        match args[idx].as_str() {
            "update-ref" => {
                idx += 1;
            }
            "--stdin" | "--batch-updates" => {
                return Err(GitAiError::Generic(
                    "trace2 cursor does not support update-ref stdin/batch updates".to_string(),
                ));
            }
            "-d" | "--delete" => {
                delete = true;
                idx += 1;
            }
            "-m" | "--message" => {
                if idx + 1 >= args.len() {
                    return Err(GitAiError::Generic(
                        "update-ref -m requires a message argument".to_string(),
                    ));
                }
                idx += 2;
            }
            "--create-reflog" | "--no-deref" => {
                idx += 1;
            }
            value if value.starts_with("--message=") => {
                idx += 1;
            }
            value if value.starts_with('-') => {
                return Err(GitAiError::Generic(format!(
                    "trace2 cursor does not support update-ref option '{}'",
                    value
                )));
            }
            value => {
                positionals.push(value.to_string());
                idx += 1;
            }
        }
    }

    if delete {
        return match positionals.as_slice() {
            [reference] => Ok(Some(UpdateRefSpec {
                reference: reference.to_string(),
                new_oid: zero_oid(),
                old_oid: None,
            })),
            [reference, old_oid] => Ok(Some(UpdateRefSpec {
                reference: reference.to_string(),
                new_oid: zero_oid(),
                old_oid: Some(old_oid.to_string()),
            })),
            _ => Err(GitAiError::Generic(
                "update-ref delete requires <ref> [<old-oid>]".to_string(),
            )),
        };
    }

    match positionals.as_slice() {
        [reference, new_oid] => Ok(Some(UpdateRefSpec {
            reference: reference.to_string(),
            new_oid: new_oid.to_string(),
            old_oid: None,
        })),
        [reference, new_oid, old_oid] => Ok(Some(UpdateRefSpec {
            reference: reference.to_string(),
            new_oid: new_oid.to_string(),
            old_oid: Some(old_oid.to_string()),
        })),
        _ => Err(GitAiError::Generic(
            "update-ref requires <ref> <new-oid> [<old-oid>]".to_string(),
        )),
    }
}

fn parse_branch_command_spec(args: &[String]) -> BranchCommandSpec {
    let args = branch_command_args(args);
    let mut delete = false;
    let mut remote_delete = false;
    let mut rename = false;
    let mut copy = false;
    let mut list_only = false;
    let mut config_only = false;
    let mut positionals = Vec::new();
    let mut idx = 0usize;

    while idx < args.len() {
        let arg = &args[idx];
        if arg == "--" {
            positionals.extend(args[idx + 1..].iter().cloned());
            break;
        }

        match arg.as_str() {
            "-d" | "-D" | "--delete" => {
                delete = true;
                idx += 1;
            }
            "-m" | "-M" | "--move" => {
                rename = true;
                idx += 1;
            }
            "-c" | "-C" | "--copy" => {
                copy = true;
                idx += 1;
            }
            "-r" | "--remotes" => {
                remote_delete = true;
                list_only = true;
                idx += 1;
            }
            "-a" | "--all" | "--list" | "--show-current" | "--contains" | "--no-contains"
            | "--merged" | "--no-merged" => {
                list_only = true;
                idx += 1;
            }
            "--unset-upstream" | "--edit-description" | "--set-upstream" => {
                config_only = true;
                idx += 1;
            }
            "-u" | "--set-upstream-to" => {
                config_only = true;
                idx = idx.saturating_add(2);
            }
            "--points-at" | "--sort" | "--format" => {
                list_only = true;
                idx = idx.saturating_add(2);
            }
            "--color" | "--column" | "--abbrev" => {
                idx = idx.saturating_add(2);
            }
            "--track"
            | "--no-track"
            | "--create-reflog"
            | "--no-create-reflog"
            | "--recurse-submodules"
            | "--no-color"
            | "--no-column"
            | "--no-abbrev"
            | "--quiet"
            | "-q"
            | "--verbose"
            | "-v"
            | "-vv"
            | "-f"
            | "--force"
            | "-l" => {
                idx += 1;
            }
            value if value.starts_with("--set-upstream-to=") => {
                config_only = true;
                idx += 1;
            }
            value
                if value.starts_with("--points-at=")
                    || value.starts_with("--sort=")
                    || value.starts_with("--format=")
                    || value.starts_with("--contains=")
                    || value.starts_with("--no-contains=")
                    || value.starts_with("--merged=")
                    || value.starts_with("--no-merged=") =>
            {
                list_only = true;
                idx += 1;
            }
            value
                if value.starts_with("--track=")
                    || value.starts_with("--color=")
                    || value.starts_with("--column=")
                    || value.starts_with("--abbrev=") =>
            {
                idx += 1;
            }
            value if value.starts_with("--") => {
                idx += 1;
            }
            value if value.starts_with('-') => {
                apply_branch_short_options(
                    value,
                    &mut delete,
                    &mut remote_delete,
                    &mut rename,
                    &mut copy,
                    &mut list_only,
                );
                idx += branch_short_option_value_width(value);
            }
            value => {
                positionals.push(value.to_string());
                idx += 1;
            }
        }
    }

    if delete {
        let references = positionals
            .into_iter()
            .filter_map(|name| branch_ref_name(&name, remote_delete))
            .collect::<Vec<_>>();
        return if references.is_empty() {
            BranchCommandSpec::None
        } else {
            BranchCommandSpec::Delete { references }
        };
    }

    if rename {
        return match positionals.as_slice() {
            [new_name] => branch_ref_name(new_name, false)
                .map(|new_reference| BranchCommandSpec::Rename {
                    old_reference: None,
                    new_reference,
                })
                .unwrap_or(BranchCommandSpec::None),
            [old_name, new_name] => {
                match (
                    branch_ref_name(old_name, false),
                    branch_ref_name(new_name, false),
                ) {
                    (Some(old_reference), Some(new_reference)) => BranchCommandSpec::Rename {
                        old_reference: Some(old_reference),
                        new_reference,
                    },
                    _ => BranchCommandSpec::None,
                }
            }
            _ => BranchCommandSpec::None,
        };
    }

    if copy {
        return match positionals.as_slice() {
            [new_name] => branch_ref_name(new_name, false)
                .map(|new_reference| BranchCommandSpec::Copy {
                    old_reference: None,
                    new_reference,
                })
                .unwrap_or(BranchCommandSpec::None),
            [old_name, new_name] => {
                match (
                    branch_ref_name(old_name, false),
                    branch_ref_name(new_name, false),
                ) {
                    (Some(old_reference), Some(new_reference)) => BranchCommandSpec::Copy {
                        old_reference: Some(old_reference),
                        new_reference,
                    },
                    _ => BranchCommandSpec::None,
                }
            }
            _ => BranchCommandSpec::None,
        };
    }

    if config_only || list_only {
        return BranchCommandSpec::None;
    }

    positionals
        .first()
        .and_then(|name| branch_ref_name(name, false))
        .map(|reference| BranchCommandSpec::CreateOrReset { reference })
        .unwrap_or(BranchCommandSpec::None)
}

fn branch_command_args(args: &[String]) -> &[String] {
    if args.first().is_some_and(|arg| arg == "branch") {
        &args[1..]
    } else {
        args
    }
}

fn apply_branch_short_options(
    value: &str,
    delete: &mut bool,
    remote_delete: &mut bool,
    rename: &mut bool,
    copy: &mut bool,
    list_only: &mut bool,
) {
    for flag in value.trim_start_matches('-').chars() {
        match flag {
            'd' | 'D' => *delete = true,
            'r' => {
                *remote_delete = true;
                *list_only = true;
            }
            'm' | 'M' => *rename = true,
            'c' | 'C' => *copy = true,
            'a' => *list_only = true,
            _ => {}
        }
    }
}

fn branch_short_option_value_width(value: &str) -> usize {
    if value == "-u" { 2 } else { 1 }
}

fn branch_ref_name(name: &str, remote: bool) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed == "--" || trimmed.starts_with('-') {
        return None;
    }
    if trimmed.starts_with("refs/heads/") || trimmed.starts_with("refs/remotes/") {
        return Some(trimmed.to_string());
    }
    if trimmed.starts_with("refs/") {
        return None;
    }
    if remote {
        Some(format!("refs/remotes/{}", trimmed))
    } else {
        Some(format!("refs/heads/{}", trimmed))
    }
}

fn parse_branch_lifecycle_message(
    kind: BranchLifecycleKind,
    message: &str,
) -> Option<(String, String)> {
    let prefix = match kind {
        BranchLifecycleKind::Rename => "Branch: renamed ",
        BranchLifecycleKind::Copy => "Branch: copied ",
    };
    let rest = message.strip_prefix(prefix)?;
    let (old_reference, new_reference) = rest.split_once(" to ")?;
    Some((old_reference.to_string(), new_reference.to_string()))
}

fn working_log_base_oids(worktree: &Path) -> HashSet<String> {
    let mut out = HashSet::new();
    let Ok(repo) = find_repository_in_path(&worktree.to_string_lossy()) else {
        return out;
    };
    let Ok(entries) = fs::read_dir(&repo.storage.working_logs) else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name == "initial" {
            out.insert("0000000000000000000000000000000000000000".to_string());
        } else if valid_non_zero_oid(&name) {
            out.insert(name);
        }
    }
    out
}

fn checkout_is_path_checkout(cmd: &NormalizedCommand) -> bool {
    let args = command_args(cmd);
    args.iter().any(|arg| arg == "--")
        || args
            .iter()
            .any(|arg| arg.starts_with("--pathspec") || arg == "--ours" || arg == "--theirs")
}

fn stash_command_args(args: &[String]) -> &[String] {
    if args.first().is_some_and(|arg| arg == "stash") {
        &args[1..]
    } else {
        args
    }
}

fn stash_target_index(target: Option<&String>) -> Option<usize> {
    let target = target.map(String::as_str).unwrap_or("stash@{0}");
    if matches!(target, "stash" | "refs/stash") {
        return Some(0);
    }
    target
        .strip_prefix("stash@{")
        .and_then(|value| value.strip_suffix('}'))
        .and_then(|value| value.parse::<usize>().ok())
}

fn command_uses_ref_cursor(primary: &str) -> bool {
    matches!(
        primary,
        "commit"
            | "revert"
            | "reset"
            | "checkout"
            | "switch"
            | "merge"
            | "cherry-pick"
            | "rebase"
            | "pull"
            | "branch"
            | "stash"
            | "update-ref"
    )
}

fn command_can_move_refs_on_nonzero(primary: Option<&str>) -> bool {
    matches!(
        primary,
        Some("checkout" | "switch" | "stash" | "rebase" | "pull" | "branch")
    )
}

fn message_matches(message: &str, prefixes: &[&str]) -> bool {
    prefixes.is_empty() || prefixes.iter().any(|prefix| message.starts_with(prefix))
}

fn valid_ref_transition(old: &str, new: &str) -> bool {
    is_valid_git_oid(old) && is_valid_git_oid(new) && old != new
}

fn valid_non_zero_oid(value: &str) -> bool {
    is_valid_git_oid(value) && !value.chars().all(|ch| ch == '0')
}

fn zero_oid() -> String {
    "0000000000000000000000000000000000000000".to_string()
}

fn entry_to_ref_change(entry: &CursorEntry) -> RefChange {
    RefChange {
        reference: entry.reference.clone(),
        old: entry.old.clone(),
        new: entry.new.clone(),
    }
}

fn dedup_ref_changes(changes: &mut Vec<RefChange>) {
    let mut seen = HashSet::new();
    changes.retain(|change| {
        seen.insert((
            change.reference.clone(),
            change.old.clone(),
            change.new.clone(),
        ))
    });
}

fn common_key(reference: &str) -> String {
    format!("common:{}", reference)
}

fn head_key(git_dir: &Path) -> String {
    let normalized = git_dir
        .canonicalize()
        .unwrap_or_else(|_| git_dir.to_path_buf())
        .to_string_lossy()
        .to_string();
    format!("worktree:{}:HEAD", normalized)
}
