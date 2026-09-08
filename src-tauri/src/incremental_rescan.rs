use crate::fsevents_callback::CollectedFseventsChange;
use crate::incremental_paths::normalize_relative;
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

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

/// 置換範囲を部分木へ拡大し、祖先で統合する。
///
/// exact targetのまま置換すると、directoryのrename・削除・fileへの型変更で
/// 古い子孫がsnapshotに残る。差分適用の置換単位は必ず部分木にする。
pub fn escalate_to_subtrees(
    targets: &[IncrementalRescanTarget],
) -> Result<Vec<IncrementalRescanTarget>, String> {
    let mut paths = BTreeSet::new();
    let mut includes_root = false;
    for target in targets {
        let path = normalize_relative(&target.relative_path)
            .ok_or_else(|| "部分再走査targetが不正です".to_owned())?;
        if path == Path::new(".") {
            includes_root = true;
        } else {
            paths.insert(path);
        }
    }
    let paths = if includes_root {
        vec![PathBuf::from(".")]
    } else {
        collapse_targets(paths)
    };
    Ok(paths
        .into_iter()
        .map(|relative_path| IncrementalRescanTarget {
            relative_path,
            recursive: true,
        })
        .collect())
}

pub fn plan_incremental_rescan(
    changes: &[CollectedFseventsChange],
    rescan_subtrees: bool,
    max_targets: usize,
) -> IncrementalRescanPlan {
    let mut paths = BTreeSet::new();
    let mut includes_root = false;
    for change in changes {
        let Some(path) = normalize_relative(&change.relative_path) else {
            return IncrementalRescanPlan::Full {
                reason: FullRescanReason::InvalidChangePath,
            };
        };
        if path == Path::new(".") {
            includes_root = true;
            paths.clear();
        } else if !includes_root {
            paths.insert(path);
        }
        // rootがあっても、後続の不正pathを検査するまで計画を確定しない。
    }
    if includes_root {
        return if max_targets == 0 {
            IncrementalRescanPlan::Full {
                reason: FullRescanReason::TooManyTargets,
            }
        } else {
            IncrementalRescanPlan::Partial {
                targets: vec![IncrementalRescanTarget {
                    relative_path: PathBuf::from("."),
                    recursive: true,
                }],
            }
        };
    }
    // exact targetは子孫を走査・置換しない。祖先による統合は再帰時だけ行う。
    let paths = if rescan_subtrees {
        collapse_targets(paths)
    } else {
        paths.into_iter().collect()
    };
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

    fn partial(targets: &[(&str, bool)]) -> IncrementalRescanPlan {
        IncrementalRescanPlan::Partial {
            targets: targets
                .iter()
                .map(|(path, recursive)| IncrementalRescanTarget {
                    relative_path: PathBuf::from(*path),
                    recursive: *recursive,
                })
                .collect(),
        }
    }

    #[test]
    fn preserves_descendants_of_exact_targets() {
        let changes = [change("dir/file"), change("dir"), change("dir/file")];
        assert_eq!(
            plan_incremental_rescan(&changes, false, 8),
            partial(&[("dir", false), ("dir/file", false)])
        );
    }

    #[test]
    fn exact_descendants_count_toward_the_target_limit() {
        assert_eq!(
            plan_incremental_rescan(&[change("dir"), change("dir/file")], false, 1),
            IncrementalRescanPlan::Full {
                reason: FullRescanReason::TooManyTargets,
            }
        );
        assert_eq!(
            plan_incremental_rescan(&[change("dir"), change("dir/file")], true, 1),
            partial(&[("dir", true)])
        );
    }

    #[test]
    fn validates_unsafe_changes_before_and_after_root() {
        for unsafe_path in ["", "../outside", "/outside", "dir/../outside"] {
            for recursive in [false, true] {
                for root_first in [false, true] {
                    let changes = if root_first {
                        vec![change("."), change(unsafe_path)]
                    } else {
                        vec![change(unsafe_path), change(".")]
                    };
                    assert_eq!(
                        plan_incremental_rescan(&changes, recursive, 8),
                        IncrementalRescanPlan::Full {
                            reason: FullRescanReason::InvalidChangePath,
                        }
                    );
                }
            }
        }
    }

    #[test]
    fn zero_budget_allows_only_an_empty_plan() {
        for recursive in [false, true] {
            assert_eq!(plan_incremental_rescan(&[], recursive, 0), partial(&[]));
            for path in [".", "file", "dir/file"] {
                assert_eq!(
                    plan_incremental_rescan(&[change(path)], recursive, 0),
                    IncrementalRescanPlan::Full {
                        reason: FullRescanReason::TooManyTargets,
                    }
                );
            }
        }
    }

    #[test]
    fn root_collapses_valid_changes_independent_of_order() {
        for recursive in [false, true] {
            for changes in [
                vec![change("."), change("dir/file"), change(".")],
                vec![change("dir/file"), change("."), change(".")],
            ] {
                assert_eq!(
                    plan_incremental_rescan(&changes, recursive, 1),
                    partial(&[(".", true)])
                );
            }
        }
    }

    #[test]
    fn does_not_collapse_sibling_with_a_common_text_prefix() {
        assert_eq!(
            plan_incremental_rescan(&[change("dir/file"), change("directory/file")], true, 2),
            partial(&[("dir/file", true), ("directory/file", true)])
        );
    }

    #[test]
    fn plans_are_independent_of_input_order() {
        let orders = [
            ["dir", "dir/file", "other"],
            ["dir", "other", "dir/file"],
            ["dir/file", "dir", "other"],
            ["dir/file", "other", "dir"],
            ["other", "dir", "dir/file"],
            ["other", "dir/file", "dir"],
        ];
        for recursive in [false, true] {
            let expected = if recursive {
                partial(&[("dir", true), ("other", true)])
            } else {
                partial(&[("dir", false), ("dir/file", false), ("other", false)])
            };
            for order in &orders {
                let changes: Vec<_> = order.iter().map(|path| change(path)).collect();
                assert_eq!(plan_incremental_rescan(&changes, recursive, 8), expected);
            }
        }
    }

    #[test]
    fn every_valid_change_remains_covered_by_the_plan() {
        let paths = ["a", "a/b", "a/b/c", "ab", "z"];
        for mask in 0..(1_usize << paths.len()) {
            let changes: Vec<_> = paths
                .iter()
                .enumerate()
                .filter(|(index, _)| mask & (1_usize << *index) != 0)
                .map(|(_, path)| change(path))
                .collect();
            for recursive in [false, true] {
                let IncrementalRescanPlan::Partial { targets } =
                    plan_incremental_rescan(&changes, recursive, paths.len())
                else {
                    panic!("有効な変更は上限内の部分計画になる必要があります");
                };
                for change in &changes {
                    assert!(targets.iter().any(|target| {
                        change.relative_path == target.relative_path
                            || (target.recursive
                                && change.relative_path.starts_with(&target.relative_path))
                    }));
                }
            }
        }
    }
}
