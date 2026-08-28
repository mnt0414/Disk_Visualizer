use crate::fsevents_callback::CollectedFseventsChange;
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FullRescanReason {
    InvalidChangePath,
    TooManyTargets,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IncrementalRescanTarget {
    pub relative_path: PathBuf,
    pub recursive: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IncrementalRescanPlan {
    Partial {
        targets: Vec<IncrementalRescanTarget>,
    },
    Full {
        reason: FullRescanReason,
    },
}

fn normalize_relative(path: &Path) -> Option<PathBuf> {
    if path.as_os_str().is_empty() {
        return None;
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(value) => normalized.push(value),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if normalized.as_os_str().is_empty() {
        Some(PathBuf::from("."))
    } else {
        Some(normalized)
    }
}

fn collapse_targets(paths: BTreeSet<PathBuf>) -> Vec<PathBuf> {
    let mut collapsed: Vec<PathBuf> = Vec::new();
    for path in paths {
        if collapsed.iter().any(|ancestor| path.starts_with(ancestor)) {
            continue;
        }
        collapsed.retain(|descendant| !descendant.starts_with(&path));
        collapsed.push(path);
    }
    collapsed
}

pub fn plan_incremental_rescan(
    changes: &[CollectedFseventsChange],
    rescan_subtrees: bool,
    max_targets: usize,
) -> IncrementalRescanPlan {
    let mut paths = BTreeSet::new();
    for change in changes {
        let Some(path) = normalize_relative(&change.relative_path) else {
            return IncrementalRescanPlan::Full {
                reason: FullRescanReason::InvalidChangePath,
            };
        };
        if path == Path::new(".") {
            return IncrementalRescanPlan::Partial {
                targets: vec![IncrementalRescanTarget {
                    relative_path: path,
                    recursive: true,
                }],
            };
        }
        paths.insert(path);
    }
    let paths = collapse_targets(paths);
    if paths.len() > max_targets {
        return IncrementalRescanPlan::Full {
            reason: FullRescanReason::TooManyTargets,
        };
    }
    IncrementalRescanPlan::Partial {
        targets: paths
            .into_iter()
            .map(|relative_path| IncrementalRescanTarget {
                relative_path,
                recursive: rescan_subtrees,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::macos_fsevents::FseventsEvent;

    fn change(path: &str) -> CollectedFseventsChange {
        CollectedFseventsChange {
            relative_path: PathBuf::from(path),
            event: FseventsEvent {
                event_id: 11,
                flags: 0,
            },
        }
    }

    #[test]
    fn plans_exact_targets_for_normal_changes() {
        let plan = plan_incremental_rescan(&[change("a.txt"), change("dir/b.txt")], false, 8);
        assert_eq!(
            plan,
            IncrementalRescanPlan::Partial {
                targets: vec![
                    IncrementalRescanTarget {
                        relative_path: "a.txt".into(),
                        recursive: false,
                    },
                    IncrementalRescanTarget {
                        relative_path: "dir/b.txt".into(),
                        recursive: false,
                    },
                ],
            }
        );
    }

    #[test]
    fn marks_subtree_targets_as_recursive() {
        let plan = plan_incremental_rescan(&[change("dir")], true, 8);
        assert_eq!(
            plan,
            IncrementalRescanPlan::Partial {
                targets: vec![IncrementalRescanTarget {
                    relative_path: "dir".into(),
                    recursive: true,
                }],
            }
        );
    }

    #[test]
    fn collapses_duplicates_and_descendants() {
        let plan = plan_incremental_rescan(
            &[change("dir/file"), change("dir"), change("dir/file")],
            true,
            8,
        );
        assert_eq!(
            plan,
            IncrementalRescanPlan::Partial {
                targets: vec![IncrementalRescanTarget {
                    relative_path: "dir".into(),
                    recursive: true,
                }],
            }
        );
    }

    #[test]
    fn represents_volume_root_as_one_recursive_target() {
        let plan = plan_incremental_rescan(&[change("nested"), change(".")], false, 8);
        assert_eq!(
            plan,
            IncrementalRescanPlan::Partial {
                targets: vec![IncrementalRescanTarget {
                    relative_path: ".".into(),
                    recursive: true,
                }],
            }
        );
    }

    #[test]
    fn fails_closed_for_unsafe_paths_and_target_overflow() {
        assert_eq!(
            plan_incremental_rescan(&[change("../outside")], false, 8),
            IncrementalRescanPlan::Full {
                reason: FullRescanReason::InvalidChangePath,
            }
        );
        assert_eq!(
            plan_incremental_rescan(&[change("a"), change("b")], false, 1),
            IncrementalRescanPlan::Full {
                reason: FullRescanReason::TooManyTargets,
            }
        );
    }
}
