//! Exact-line program-counter candidate selection.
//!
//! A source line can map to several statement-boundary PCs. The first row is
//! not necessarily a valid observation point: a local may only have a DWARF
//! location on a later row. This policy is independent of DWARF and language.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PcCandidate<T> {
    pub pc: u64,
    pub value: T,
    pub available: usize,
    pub requested: usize,
}

impl<T> PcCandidate<T> {
    pub fn complete(&self) -> bool {
        self.available == self.requested
    }
}

/// Select a complete candidate when possible, otherwise retain the candidate
/// with the largest supported subset. When several rows expose the same
/// fields, prefer the earliest PC on the requested line. This avoids probing
/// after a compiler has moved a local into a call argument or teardown slot;
/// location validity alone does not guarantee the value remains initialized.
pub fn select_best<T>(
    candidates: impl IntoIterator<Item = PcCandidate<T>>,
) -> Option<PcCandidate<T>> {
    let mut best = None;
    for candidate in candidates {
        let replace = best.as_ref().is_none_or(|current: &PcCandidate<T>| {
            candidate.available > current.available
                || (candidate.complete() && !current.complete())
                || (candidate.available == current.available && candidate.pc < current.pc)
        });
        if replace {
            best = Some(candidate);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn later_pc_with_live_location_wins() {
        let selected = select_best([
            PcCandidate {
                pc: 0x10,
                value: "first",
                available: 0,
                requested: 1,
            },
            PcCandidate {
                pc: 0x20,
                value: "live",
                available: 1,
                requested: 1,
            },
        ])
        .unwrap();
        assert_eq!((selected.pc, selected.value), (0x20, "live"));
    }

    #[test]
    fn partial_candidate_is_retained_when_no_complete_candidate_exists() {
        let selected = select_best([
            PcCandidate {
                pc: 0x10,
                value: 1,
                available: 1,
                requested: 2,
            },
            PcCandidate {
                pc: 0x20,
                value: 2,
                available: 0,
                requested: 2,
            },
        ])
        .unwrap();
        assert_eq!(selected.pc, 0x10);
    }

    #[test]
    fn equal_complete_candidates_prefer_earlier_live_statement() {
        let selected = select_best([
            PcCandidate {
                pc: 0x30,
                value: "late",
                available: 1,
                requested: 1,
            },
            PcCandidate {
                pc: 0x20,
                value: "early",
                available: 1,
                requested: 1,
            },
        ])
        .unwrap();
        assert_eq!((selected.pc, selected.value), (0x20, "early"));
    }
}
