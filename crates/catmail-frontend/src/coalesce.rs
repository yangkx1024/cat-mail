//! Reading something again as it changes, one read at a time: the list's, the
//! sidebar's reads of the accounts, the tray's count.

/// One read at a time. Asking while one runs asks for one more after it, so the last
/// read always starts after the last change; only replacing the UI's task wouldn't do,
/// as the engine finishes a read it was handed anyway.
#[derive(Debug, Default)]
pub struct Coalesce {
    running: bool,
    again: bool,
}

impl Coalesce {
    /// Whether to start reading now; if not, another read follows the one running.
    pub fn start(&mut self) -> bool {
        if self.running {
            self.again = true;
            return false;
        }
        self.running = true;
        true
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    /// The read is done: whether to start another.
    pub fn finish(&mut self) -> bool {
        self.running = false;
        std::mem::take(&mut self.again)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asking_during_a_read_reads_once_more() {
        let mut reads = Coalesce::default();
        assert!(reads.start());
        assert!(!reads.start());
        assert!(!reads.start());
        assert!(reads.finish());
        assert!(reads.start());
        assert!(!reads.finish());
        assert!(reads.start());
    }
}
