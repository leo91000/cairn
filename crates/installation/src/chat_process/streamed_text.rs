const MINIMUM_GROWTH: usize = 32;
const SNAPSHOT_SCALE: usize = 256;

/// Cumulative snapshots remain compatible with installed clients. Wait for
/// fresh bytes before copying the whole answer again: time-only throttling
/// still amplifies a slowly delivered answer quadratically.
#[derive(Default)]
pub(super) struct StreamedText {
    value: String,
    published: usize,
}

impl StreamedText {
    pub(super) fn len(&self) -> usize {
        self.value.len()
    }

    pub(super) fn append(&mut self, delta: &str) {
        self.value.push_str(delta);
    }

    pub(super) fn ready(&self) -> bool {
        let growth = self.value.len() - self.published;
        self.published == 0 || growth >= MINIMUM_GROWTH.max(self.published.div_ceil(SNAPSHOT_SCALE))
    }

    pub(super) fn publish(&mut self) -> &str {
        self.published = self.value.len();
        &self.value
    }
}

#[cfg(test)]
mod tests {
    use super::StreamedText;

    #[test]
    fn fine_deltas_bound_repeated_text_independently_of_delivery_time() {
        let mut text = StreamedText::default();
        let mut total = 0;
        let mut emitted = 0;
        let mut longest_gap = 0;
        let mut previous = 0;
        for _ in 0..6000 {
            text.append("xxxx");
            if text.ready() {
                let snapshot = text.publish();
                total += snapshot.len();
                emitted += 1;
                longest_gap = longest_gap.max(snapshot.len() - previous);
                previous = snapshot.len();
            }
        }
        // Every ordinary snapshot costs at most 257 times the newly received
        // bytes that enabled it. Forced semantic-boundary flushes are separate.
        assert!(total <= 257 * text.len());
        assert!(emitted > 1 && emitted < 6000);
        assert!(longest_gap <= 96);
        assert_eq!(text.publish(), "x".repeat(24_000));
    }

    #[test]
    fn first_text_and_forced_tail_preserve_utf8() {
        let mut text = StreamedText::default();
        text.append("é🌱");
        assert!(text.ready());
        assert_eq!(text.publish(), "é🌱");
        text.append("\n\"fin\"");
        assert!(!text.ready());
        assert_eq!(text.publish(), "é🌱\n\"fin\"");
    }
}
