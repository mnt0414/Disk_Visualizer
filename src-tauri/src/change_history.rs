use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryToken {
    Fsevents { event_id: u64 },
    Usn { journal_id: u64, next_usn: i64 },
}

impl HistoryToken {
    pub fn encode(self) -> String {
        match self {
            Self::Fsevents { event_id } => format!("fsevents:v1:{event_id}"),
            Self::Usn {
                journal_id,
                next_usn,
            } => format!("usn:v1:{journal_id}:{next_usn}"),
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        let parts = value.split(':').collect::<Vec<_>>();
        match parts.as_slice() {
            ["fsevents", "v1", event_id] => {
                let event_id = event_id
                    .parse::<u64>()
                    .map_err(|_| "FSEvents tokenのevent IDが不正です".to_owned())?;
                if event_id == 0 {
                    return Err("FSEvents tokenのevent IDは0にできません".to_owned());
                }
                Ok(Self::Fsevents { event_id })
            }
            ["usn", "v1", journal_id, next_usn] => {
                let journal_id = journal_id
                    .parse::<u64>()
                    .map_err(|_| "USN tokenのjournal IDが不正です".to_owned())?;
                let next_usn = next_usn
                    .parse::<i64>()
                    .map_err(|_| "USN tokenのnext USNが不正です".to_owned())?;
                if journal_id == 0 || next_usn < 0 {
                    return Err("USN tokenの値が保存可能な範囲外です".to_owned());
                }
                Ok(Self::Usn {
                    journal_id,
                    next_usn,
                })
            }
            _ => Err("未対応または破損した変更履歴tokenです".to_owned()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AvailableHistory {
    Fsevents {
        earliest_event_id: u64,
        latest_event_id: u64,
    },
    Usn {
        journal_id: u64,
        lowest_valid_usn: i64,
        next_usn: i64,
    },
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HistoryContinuity {
    Continuous,
    HistoryGap,
    JournalChanged,
    SourceMismatch,
    InvalidRange,
}

pub fn evaluate_continuity(
    checkpoint: HistoryToken,
    available: AvailableHistory,
) -> HistoryContinuity {
    match (checkpoint, available) {
        (
            HistoryToken::Fsevents { event_id },
            AvailableHistory::Fsevents {
                earliest_event_id,
                latest_event_id,
            },
        ) => {
            if earliest_event_id == 0 || earliest_event_id > latest_event_id || event_id > latest_event_id
            {
                HistoryContinuity::InvalidRange
            } else if event_id < earliest_event_id {
                HistoryContinuity::HistoryGap
            } else {
                HistoryContinuity::Continuous
            }
        }
        (
            HistoryToken::Usn {
                journal_id,
                next_usn: checkpoint_usn,
            },
            AvailableHistory::Usn {
                journal_id: current_journal_id,
                lowest_valid_usn,
                next_usn,
            },
        ) => {
            if lowest_valid_usn < 0 || lowest_valid_usn > next_usn || checkpoint_usn > next_usn {
                HistoryContinuity::InvalidRange
            } else if journal_id != current_journal_id {
                HistoryContinuity::JournalChanged
            } else if checkpoint_usn < lowest_valid_usn {
                HistoryContinuity::HistoryGap
            } else {
                HistoryContinuity::Continuous
            }
        }
        _ => HistoryContinuity::SourceMismatch,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_versioned_tokens() {
        let fsevents = HistoryToken::Fsevents { event_id: 42 };
        assert_eq!(HistoryToken::parse(&fsevents.encode()).unwrap(), fsevents);
        let usn = HistoryToken::Usn {
            journal_id: 7,
            next_usn: 900,
        };
        assert_eq!(HistoryToken::parse(&usn.encode()).unwrap(), usn);
    }

    #[test]
    fn rejects_unknown_or_incomplete_tokens() {
        assert!(HistoryToken::parse("fsevents:v2:42").is_err());
        assert!(HistoryToken::parse("usn:v1:7").is_err());
        assert!(HistoryToken::parse("usn:v1:0:-1").is_err());
    }

    #[test]
    fn accepts_continuous_fsevents_history() {
        assert_eq!(
            evaluate_continuity(
                HistoryToken::Fsevents { event_id: 20 },
                AvailableHistory::Fsevents {
                    earliest_event_id: 10,
                    latest_event_id: 30,
                },
            ),
            HistoryContinuity::Continuous
        );
    }

    #[test]
    fn detects_fsevents_history_gap() {
        assert_eq!(
            evaluate_continuity(
                HistoryToken::Fsevents { event_id: 9 },
                AvailableHistory::Fsevents {
                    earliest_event_id: 10,
                    latest_event_id: 30,
                },
            ),
            HistoryContinuity::HistoryGap
        );
    }

    #[test]
    fn detects_usn_journal_recreation_and_trimmed_history() {
        let checkpoint = HistoryToken::Usn {
            journal_id: 7,
            next_usn: 100,
        };
        assert_eq!(
            evaluate_continuity(
                checkpoint,
                AvailableHistory::Usn {
                    journal_id: 8,
                    lowest_valid_usn: 50,
                    next_usn: 200,
                },
            ),
            HistoryContinuity::JournalChanged
        );
        assert_eq!(
            evaluate_continuity(
                checkpoint,
                AvailableHistory::Usn {
                    journal_id: 7,
                    lowest_valid_usn: 101,
                    next_usn: 200,
                },
            ),
            HistoryContinuity::HistoryGap
        );
    }

    #[test]
    fn rejects_source_mismatch_and_invalid_ranges() {
        assert_eq!(
            evaluate_continuity(
                HistoryToken::Fsevents { event_id: 20 },
                AvailableHistory::Usn {
                    journal_id: 7,
                    lowest_valid_usn: 10,
                    next_usn: 30,
                },
            ),
            HistoryContinuity::SourceMismatch
        );
        assert_eq!(
            evaluate_continuity(
                HistoryToken::Usn {
                    journal_id: 7,
                    next_usn: 40,
                },
                AvailableHistory::Usn {
                    journal_id: 7,
                    lowest_valid_usn: 30,
                    next_usn: 20,
                },
            ),
            HistoryContinuity::InvalidRange
        );
    }
}
