use std::collections::VecDeque;

const ROLLING_WINDOW_CHARS: usize = 16 * 1024;
const HASH_BASE: u64 = 1_099_511_628_211;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RepetitionWatchdogConfig {
    pub min_block_chars: usize,
    pub max_block_chars: usize,
    pub min_repeats: usize,
    pub min_text_chars_before_trip: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RepetitionTrip {
    pub cutoff_byte_index: usize,
    pub cutoff_char_index: usize,
    pub block_char_count: usize,
    pub repeat_count: usize,
    pub accepted_text_char_count: usize,
}

#[derive(Clone, Copy, Debug)]
struct ObservedChar {
    value: char,
    byte_index: usize,
}

pub(crate) struct RepetitionWatchdog {
    config: RepetitionWatchdogConfig,
    chars: VecDeque<ObservedChar>,
    total_bytes: usize,
    total_chars: usize,
    tripped: bool,
}

impl RepetitionWatchdog {
    pub(crate) fn new(config: RepetitionWatchdogConfig) -> Self {
        let min_repeats = config.min_repeats.max(2);
        let min_block_chars = config.min_block_chars.max(1);
        let max_block_chars = config
            .max_block_chars
            .max(min_block_chars)
            .min(ROLLING_WINDOW_CHARS / min_repeats);

        Self {
            config: RepetitionWatchdogConfig {
                min_block_chars,
                max_block_chars,
                min_repeats,
                min_text_chars_before_trip: config.min_text_chars_before_trip,
            },
            chars: VecDeque::with_capacity(ROLLING_WINDOW_CHARS),
            total_bytes: 0,
            total_chars: 0,
            tripped: false,
        }
    }

    pub(crate) fn push(&mut self, text: &str) -> Option<RepetitionTrip> {
        if self.tripped {
            return None;
        }

        for (relative_byte_index, value) in text.char_indices() {
            self.chars.push_back(ObservedChar {
                value: normalize_char(value),
                byte_index: self.total_bytes + relative_byte_index,
            });
            self.total_chars += 1;
        }
        self.total_bytes += text.len();

        while self.chars.len() > ROLLING_WINDOW_CHARS {
            self.chars.pop_front();
        }

        if self.total_chars < self.config.min_text_chars_before_trip {
            return None;
        }

        let chars = self.chars.iter().copied().collect::<Vec<_>>();
        let minimum_repeated_chars = self
            .config
            .min_block_chars
            .saturating_mul(self.config.min_repeats);
        if chars.len() < minimum_repeated_chars {
            return None;
        }

        let mut powers = Vec::with_capacity(chars.len() + 1);
        let mut prefix_hashes = Vec::with_capacity(chars.len() + 1);
        powers.push(1_u64);
        prefix_hashes.push(0_u64);
        for observed in &chars {
            powers.push(powers.last().copied().unwrap_or(1).wrapping_mul(HASH_BASE));
            prefix_hashes.push(
                prefix_hashes
                    .last()
                    .copied()
                    .unwrap_or_default()
                    .wrapping_mul(HASH_BASE)
                    .wrapping_add(observed.value as u64 + 1),
            );
        }

        let maximum_period = self
            .config
            .max_block_chars
            .min(chars.len() / self.config.min_repeats);
        for period in self.config.min_block_chars..=maximum_period {
            let suffix_start = chars.len() - period;
            let suffix_hash = range_hash(&prefix_hashes, &powers, suffix_start, chars.len());

            let mut repeat_count = 1;
            while repeat_count < chars.len() / period {
                let block_end = chars.len() - repeat_count * period;
                let block_start = block_end - period;
                if range_hash(&prefix_hashes, &powers, block_start, block_end) != suffix_hash
                    || !same_chars(&chars[block_start..block_end], &chars[suffix_start..])
                {
                    break;
                }
                repeat_count += 1;
            }

            if repeat_count < self.config.min_repeats {
                continue;
            }

            let first_occurrence = chars.len() - repeat_count * period;
            let second_occurrence = first_occurrence + period;
            let repeated_suffix = &chars[first_occurrence..];
            if repeated_suffix
                .iter()
                .all(|observed| observed.value.is_whitespace())
            {
                continue;
            }

            self.tripped = true;
            return Some(RepetitionTrip {
                cutoff_byte_index: chars[second_occurrence].byte_index,
                cutoff_char_index: self.total_chars - chars.len() + second_occurrence,
                block_char_count: period,
                repeat_count,
                accepted_text_char_count: self.total_chars,
            });
        }

        None
    }

    pub(crate) fn reset(&mut self) {
        self.chars.clear();
        self.total_bytes = 0;
        self.total_chars = 0;
        self.tripped = false;
    }
}

fn normalize_char(value: char) -> char {
    value
}

fn range_hash(prefix_hashes: &[u64], powers: &[u64], start: usize, end: usize) -> u64 {
    prefix_hashes[end].wrapping_sub(prefix_hashes[start].wrapping_mul(powers[end - start]))
}

fn same_chars(left: &[ObservedChar], right: &[ObservedChar]) -> bool {
    left.iter()
        .zip(right)
        .all(|(left, right)| left.value == right.value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> RepetitionWatchdogConfig {
        RepetitionWatchdogConfig {
            min_block_chars: 4,
            max_block_chars: 32,
            min_repeats: 3,
            min_text_chars_before_trip: 12,
        }
    }

    #[test]
    fn trips_on_third_total_occurrence_and_keeps_one() {
        let mut watchdog = RepetitionWatchdog::new(config());
        let text = "normal-abcdabcdabcd";
        let trip = watchdog.push(text).expect("expected a repetition trip");

        assert_eq!(trip.block_char_count, 4);
        assert_eq!(trip.repeat_count, 3);
        assert_eq!(&text[..trip.cutoff_byte_index], "normal-abcd");
    }

    #[test]
    fn detects_repetition_across_chunk_boundaries() {
        let mut watchdog = RepetitionWatchdog::new(config());

        assert_eq!(watchdog.push("normal-ab"), None);
        assert_eq!(watchdog.push("cdabc"), None);
        let trip = watchdog.push("dabcd").expect("expected a repetition trip");

        assert_eq!(trip.block_char_count, 4);
        assert_eq!(trip.repeat_count, 3);
    }

    #[test]
    fn returns_utf8_byte_boundary() {
        let mut watchdog = RepetitionWatchdog::new(RepetitionWatchdogConfig {
            min_block_chars: 2,
            max_block_chars: 8,
            min_repeats: 3,
            min_text_chars_before_trip: 6,
        });
        let text = "前置🙂界🙂界🙂界";
        let trip = watchdog.push(text).expect("expected a repetition trip");

        assert!(text.is_char_boundary(trip.cutoff_byte_index));
        assert_eq!(&text[..trip.cutoff_byte_index], "前置🙂界");
    }

    #[test]
    fn ignores_whitespace_only_repetition() {
        let mut watchdog = RepetitionWatchdog::new(config());
        assert_eq!(watchdog.push("            "), None);
    }

    #[test]
    fn normal_text_does_not_trip() {
        let mut watchdog = RepetitionWatchdog::new(config());
        assert_eq!(
            watchdog.push("This is a normal answer with no periodic suffix."),
            None
        );
    }

    #[test]
    fn ordinary_repeated_code_structure_does_not_trip() {
        let mut watchdog = RepetitionWatchdog::new(RepetitionWatchdogConfig {
            min_block_chars: 64,
            max_block_chars: 1_024,
            min_repeats: 3,
            min_text_chars_before_trip: 256,
        });
        let code = (0..20)
            .map(|index| {
                format!(
                    ".button-{index} {{ color: red; }}\n\
                     .button-{index}:hover {{ color: blue; }}\n\
                     {{\"tool\":\"read_file\",\"id\":{index},\"status\":\"pending\"}}\n"
                )
            })
            .collect::<String>();

        assert_eq!(watchdog.push(&code), None);
    }

    #[test]
    fn production_defaults_require_more_than_three_minimum_blocks() {
        let mut watchdog = RepetitionWatchdog::new(RepetitionWatchdogConfig {
            min_block_chars: 64,
            max_block_chars: 1_024,
            min_repeats: 3,
            min_text_chars_before_trip: 256,
        });
        let block = "A".repeat(64);

        assert_eq!(watchdog.push(&block.repeat(3)), None);
        let trip = watchdog
            .push(&block)
            .expect("fourth minimum-sized repeat should trip once the text floor is met");
        assert_eq!(trip.block_char_count, 64);
        assert!(trip.repeat_count >= 3);
    }

    #[test]
    fn block_larger_than_configured_maximum_does_not_trip() {
        let block = "abcdefghijklmnopqrstuvwxyz";
        let mut watchdog = RepetitionWatchdog::new(RepetitionWatchdogConfig {
            min_block_chars: 4,
            max_block_chars: 8,
            min_repeats: 3,
            min_text_chars_before_trip: 12,
        });

        assert_eq!(watchdog.push(&block.repeat(3)), None);
    }

    #[test]
    fn rolling_storage_is_bounded() {
        let mut watchdog = RepetitionWatchdog::new(RepetitionWatchdogConfig {
            min_block_chars: 64,
            max_block_chars: 1_024,
            min_repeats: 3,
            min_text_chars_before_trip: usize::MAX,
        });

        watchdog.push(&"bounded-window-".repeat(10_000));

        assert_eq!(watchdog.chars.len(), ROLLING_WINDOW_CHARS);
    }
}
