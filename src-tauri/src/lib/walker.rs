//! Utils and actors to walk directories recursively (or not) and perform `TimeMachine` operations on demand.

use std::collections::HashMap;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use crossbeam::queue::SegQueue;
use itertools::Itertools;
use jwalk::WalkDirGeneric;
use moka::sync::Cache;
use tap::TapFallible;
use tauri::async_runtime::Sender;
use tracing::{debug, warn};

use crate::config::{Directory, Rule, WalkConfig};
use crate::skip_cache::CachedPath;
use crate::tmutil::{is_excluded, is_nodump, ExclusionAction, ExclusionActionBatch};

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
enum ExcludeState {
    /// The path is currently excluded from backups.
    Excluded,
    /// The path is currently excluded from backups.
    Included,
    /// The exclude state of the path is unknown (conflict between TimeMachine and NODUMP).
    Inconsistent,
}

impl ExcludeState {
    pub fn is_excluded(&self) -> bool {
        matches!(self, Self::Excluded)
    }
}

/// Shallow per-entry snapshot used by `generate_diff`.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
struct ShallowEntry {
    state: ExcludeState,
    /// Whether the entry is a real directory. Captured from the entry's file type
    /// without following symlinks, so dot-symlinks are not hidden-matched.
    is_dir: bool,
}

/// Whether the entry name is dot-prefixed (hidden in the unix sense).
fn is_hidden(name: &Path) -> bool {
    name.as_os_str().as_bytes().first() == Some(&b'.')
}

fn check_f(support_dump: bool) -> fn(&Path) -> std::io::Result<ExcludeState> {
    if support_dump {
        |path| {
            let excluded = is_excluded(path)?;
            let nodump = is_nodump(path)?;
            Ok(match (excluded, nodump) {
                (true, true) => ExcludeState::Excluded,
                (false, false) => ExcludeState::Included,
                _ => ExcludeState::Inconsistent,
            })
        }
    } else {
        |path| {
            let excluded = is_excluded(path)?;
            Ok(if excluded {
                ExcludeState::Excluded
            } else {
                ExcludeState::Included
            })
        }
    }
}

/// Walk through a directory with given rules recursively and return an exclusion action plan.
#[allow(clippy::needless_pass_by_value)]
#[must_use]
pub fn walk_recursive(
    config: WalkConfig,
    support_dump: bool,
    curr_tx: Sender<PathBuf>,
    found: Arc<AtomicUsize>,
    abort: Arc<AtomicBool>,
) -> ExclusionActionBatch {
    let check = check_f(support_dump);

    let batch_queue = Arc::new(SegQueue::new());
    {
        let batch_queue = batch_queue.clone();
        let Ok(root) = config.root() else {
            return ExclusionActionBatch::default();
        };
        let counter = AtomicUsize::new(0);
        WalkDirGeneric::<(_, ())>::new(root)
            .root_read_dir_state(config)
            .skip_hidden(false)
            .process_read_dir({
                let abort = abort.clone();
                move |_, path, config, children| {
                    // Remove effect-less directories & skips.
                    config.directories.retain(|directory| {
                        path.starts_with(&directory.path) || directory.path.starts_with(path)
                    });
                    config.skips.retain(|skip| skip.starts_with(path));

                    if config.directories.is_empty() || abort.load(Ordering::Relaxed) {
                        // There's no need to go deeper.
                        for child in children.iter_mut().filter_map(|child| child.as_mut().ok()) {
                            child.read_children_path = None;
                        }
                        return;
                    }

                    if counter
                        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |i| {
                            Some(if i > 1000 { 0 } else { i + 1 })
                        })
                        .expect("f never returns None")
                        == 0
                    {
                        if let Err(e) = curr_tx.try_send(path.to_path_buf()) {
                            warn!("Failed to send current path: {}", e);
                        }
                    }

                    // Acquire excluded state.
                    let children = children
                        .iter_mut()
                        .filter_map(|entry| {
                            entry
                                .as_mut()
                                .tap_err(|e| warn!("Error when scanning dir {:?}: {}", path, e))
                                .ok()
                        })
                        .filter_map(|entry| {
                            let path = entry.path();
                            if config.skips.contains(&path) {
                                // Skip this entry in all preceding procedures and scans.
                                entry.read_children_path = None;
                                None
                            } else {
                                let is_dir = entry.file_type.is_dir();
                                Some((entry, check(&path).ok()?, is_dir))
                            }
                        })
                        .collect_vec();

                    // Generate diff.
                    let shallow_list: HashMap<_, _> = children
                        .iter()
                        .map(|(path, state, is_dir)| {
                            (
                                PathBuf::from(path.file_name().to_os_string()),
                                ShallowEntry {
                                    state: *state,
                                    is_dir: *is_dir,
                                },
                            )
                        })
                        .collect();
                    let diff = generate_diff(path, &shallow_list, &*config.directories);
                    found.fetch_add(diff.count(), Ordering::Relaxed);

                    // Exclude already excluded or uncovered children.
                    for (entry, state, _) in children {
                        let path = entry.path();
                        if (state.is_excluded() && !diff.remove.contains(&path))
                            || diff.add.contains(&path)
                        {
                            entry.read_children_path = None;
                        }
                    }
                    batch_queue.push(diff);
                }
            })
            .into_iter()
            .for_each(|_| {});
    }
    if abort.load(Ordering::Relaxed) {
        // Aborted, the return value is irrelevant.
        return ExclusionActionBatch::default();
    }
    let mut actions = ExclusionActionBatch::default();
    while let Some(action) = batch_queue.pop() {
        actions += action;
    }
    actions
}

/// Walk through a directory with given rules non-recursively and return an exclusion action plan.
#[must_use]
pub fn walk_non_recursive(
    root: &Path,
    config: &WalkConfig,
    support_dump: bool,
    skip_cache: &Cache<PathBuf, ()>,
) -> ExclusionActionBatch {
    let check = check_f(support_dump);

    if skip_cache.get::<CachedPath>(root.into()).is_some() {
        // Skip cache hit, early exit.
        return ExclusionActionBatch::default();
    }

    if fs::symlink_metadata(root).is_err() {
        // The path vanished before we walked it (e.g. deleted in a storm):
        // return an empty batch before any ancestors getxattr.
        return ExclusionActionBatch::default();
    }

    if config.skips.iter().any(|skip| root.starts_with(skip)) {
        // The directory should be skipped.
        skip_cache.insert(root.to_path_buf(), ());
        return ExclusionActionBatch::default();
    }

    let mut directories = config
        .directories
        .iter()
        .filter(|directory| root.starts_with(&directory.path) || directory.path.starts_with(root))
        .peekable();
    if directories.peek().is_none() {
        // There's no need to scan because no rules is applicable.
        skip_cache.insert(root.to_path_buf(), ());
        return ExclusionActionBatch::default();
    }

    if root
        .ancestors()
        .any(|path| check(path).map(|s| s.is_excluded()).unwrap_or(false))
    {
        // One of its parents is excluded.
        // Note that we don't put this dir into cache because the exclusion state of ancestors is unknown.
        return ExclusionActionBatch::default();
    }

    debug!("Walk through {:?}", root);
    match fs::read_dir(root) {
        Ok(dir) => {
            let shallow_list: HashMap<_, _> = dir
                .filter_map(|entry| {
                    entry
                        .tap_err(|e| warn!("Error when scanning dir {:?}: {}", root, e))
                        .ok()
                })
                .filter_map(|entry| {
                    let path = entry.path();
                    if config.skips.contains(&path) {
                        // Skip this entry in all preceding procedures and scans.
                        None
                    } else {
                        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                        Some((
                            PathBuf::from(path.file_name().expect("file name").to_os_string()),
                            check(&path).ok()?,
                            is_dir,
                        ))
                    }
                })
                .map(|(name, state, is_dir)| (name, ShallowEntry { state, is_dir }))
                .collect();
            generate_diff(root, &shallow_list, directories)
        }
        Err(e) => {
            warn!("Error when scanning dir {:?}: {}", root, e);
            ExclusionActionBatch::default()
        }
    }
}

fn generate_diff<'a, 'b>(
    cwd: &'a Path,
    shallow_list: &'a HashMap<PathBuf, ShallowEntry>,
    directories: impl IntoIterator<Item = &'b Directory>,
) -> ExclusionActionBatch {
    let candidate_rules: Vec<&Rule> = directories
        .into_iter()
        .filter(|directory| directory.path.starts_with(cwd) || cwd.starts_with(&directory.path))
        .flat_map(|directory| &directory.rules)
        .collect();
    shallow_list
        .iter()
        .filter_map(|(name, entry)| {
            // `protects` vetoes Add only: a protected entry carrying a stale
            // exclusion is still cleaned up via Remove.
            let protected = candidate_rules
                .iter()
                .any(|r| r.protects.as_deref().unwrap_or(&[]).contains(name));
            let expected_excluded = !protected
                && candidate_rules.iter().any(|rule| {
                    let matched = rule.excludes.contains(name)
                        || (rule.exclude_hidden.unwrap_or(false)
                            && entry.is_dir
                            && is_hidden(name));
                    matched
                        && (rule.if_exists.is_empty()
                            || rule
                                .if_exists
                                .iter()
                                .any(|if_exist| shallow_list.contains_key(if_exist.as_path())))
                });
            match (expected_excluded, entry.state) {
                (true, ExcludeState::Included | ExcludeState::Inconsistent) => {
                    Some(ExclusionAction::Add(cwd.join(name)))
                }
                (false, ExcludeState::Excluded | ExcludeState::Inconsistent) => {
                    Some(ExclusionAction::Remove(cwd.join(name)))
                }
                _ => None,
            }
        })
        .into()
}

#[cfg(test)]
mod test {
    use std::collections::{HashMap, HashSet};
    use std::path::{Path, PathBuf};
    use std::str::FromStr;

    use super::{generate_diff, walk_non_recursive, ExcludeState, ShallowEntry};
    use crate::config::{Directory, Rule, WalkConfig};
    use crate::skip_cache::SkipCache;
    use crate::tmutil::ExclusionActionBatch;

    fn rule(
        excludes: &[&str],
        if_exists: &[&str],
        exclude_hidden: Option<bool>,
        protects: &[&str],
    ) -> Rule {
        Rule {
            excludes: excludes
                .iter()
                .map(|name| PathBuf::from_str(name).unwrap())
                .collect(),
            if_exists: if_exists
                .iter()
                .map(|name| PathBuf::from_str(name).unwrap())
                .collect(),
            exclude_hidden,
            protects: Some(
                protects
                    .iter()
                    .map(|name| PathBuf::from_str(name).unwrap())
                    .collect(),
            ),
        }
    }

    fn shallow_list(entries: &[(&str, ExcludeState, bool)]) -> HashMap<PathBuf, ShallowEntry> {
        entries
            .iter()
            .map(|(name, state, is_dir)| {
                (
                    PathBuf::from_str(name).unwrap(),
                    ShallowEntry {
                        state: *state,
                        is_dir: *is_dir,
                    },
                )
            })
            .collect()
    }

    fn generate(
        shallow_list: &HashMap<PathBuf, ShallowEntry>,
        rules: Vec<Rule>,
    ) -> ExclusionActionBatch {
        // Directory path "/" makes every rule applicable to the cwd "/".
        generate_diff(
            Path::new("/"),
            shallow_list,
            &[Directory {
                path: PathBuf::from("/"),
                rules,
            }],
        )
    }

    #[test]
    fn hidden_dot_dir_matched() {
        let list = shallow_list(&[(".hidden", ExcludeState::Included, true)]);
        let batch = generate(&list, vec![rule(&[], &[], Some(true), &[])]);
        assert_eq!(batch.add, vec![PathBuf::from("/.hidden")]);
        assert!(batch.remove.is_empty());
    }

    #[test]
    fn dot_file_not_matched() {
        let list = shallow_list(&[(".hidden_file", ExcludeState::Included, false)]);
        let batch = generate(&list, vec![rule(&[], &[], Some(true), &[])]);
        assert!(batch.add.is_empty());
        assert!(batch.remove.is_empty());
    }

    #[test]
    fn dot_symlink_to_dir_not_matched() {
        // A dot-prefixed symlink is not a real directory: `is_dir` is captured without
        // following symlinks, so it must not be hidden-matched.
        let list = shallow_list(&[(".link", ExcludeState::Included, false)]);
        let batch = generate(&list, vec![rule(&[], &[], Some(true), &[])]);
        assert!(batch.add.is_empty());
        assert!(batch.remove.is_empty());
    }

    #[test]
    fn protects_blocks_add_but_not_remove() {
        let protected = rule(&["protected"], &[], None, &["protected"]);

        // Protected entry without stale exclusion: never Add-ed.
        let list = shallow_list(&[("protected", ExcludeState::Included, true)]);
        let batch = generate(&list, vec![protected.clone()]);
        assert!(batch.add.is_empty());
        assert!(batch.remove.is_empty());

        // Protected entry carrying a stale exclusion: Add blocked, Remove still produced.
        let list = shallow_list(&[("protected", ExcludeState::Excluded, true)]);
        let batch = generate(&list, vec![protected]);
        assert!(batch.add.is_empty());
        assert_eq!(batch.remove, vec![PathBuf::from("/protected")]);
    }

    #[test]
    fn if_exists_gates_hidden_matching() {
        // Marker absent: hidden matching is gated off.
        let list = shallow_list(&[(".hidden", ExcludeState::Included, true)]);
        let gated = rule(&[], &["marker"], Some(true), &[]);
        let batch = generate(&list, vec![gated.clone()]);
        assert!(batch.add.is_empty());
        assert!(batch.remove.is_empty());

        // Marker present: the dot-directory gains the exclusion.
        let list = shallow_list(&[
            (".hidden", ExcludeState::Included, true),
            ("marker", ExcludeState::Included, false),
        ]);
        let batch = generate(&list, vec![gated]);
        assert_eq!(batch.add, vec![PathBuf::from("/.hidden")]);
        assert!(batch.remove.is_empty());
    }

    #[test]
    fn hidden_disabled_never_matches() {
        let list = shallow_list(&[(".hidden", ExcludeState::Included, true)]);
        for exclude_hidden in [None, Some(false)] {
            let batch = generate(&list, vec![rule(&[], &[], exclude_hidden, &[])]);
            assert!(batch.add.is_empty());
            assert!(batch.remove.is_empty());
        }
    }

    #[test]
    fn excludes_name_still_matches() {
        // Name-based excludes keep working through the updated matching logic.
        let list = shallow_list(&[("build", ExcludeState::Included, true)]);
        let batch = generate(&list, vec![rule(&["build"], &[], None, &[])]);
        assert_eq!(batch.add, vec![PathBuf::from("/build")]);
    }

    #[test]
    fn deleted_path_returns_empty_batch() {
        // A path reported by FSEvents may vanish before we walk it. The fast
        // path (`fs::symlink_metadata` error) must return an empty batch
        // before any ancestors `getxattr`, without erroring.
        let temp_dir = tempfile::TempDir::new().unwrap();
        let deleted = temp_dir.path().join("deleted-dir");
        assert!(!deleted.exists());

        let config = WalkConfig {
            directories: vec![Directory {
                path: temp_dir.path().to_path_buf(),
                rules: vec![rule(&["entry"], &[], None, &[])],
            }],
            skips: HashSet::new(),
        };
        let batch = walk_non_recursive(&deleted, &config, false, &SkipCache::default());
        assert!(batch.add.is_empty());
        assert!(batch.remove.is_empty());
    }
}
