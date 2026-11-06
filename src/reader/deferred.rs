use std::io::BufRead;

#[derive(Debug, Default, PartialEq, Eq, Hash)]
pub(super) enum Deferred {
    #[default]
    None,
    Staged(Action),
}

impl Deferred {
    #[inline]
    pub(super) fn stage(&mut self, action: Action) {
        *self = Self::Staged(action);
    }

    #[inline]
    pub(super) fn commit<R: BufRead + ?Sized>(&mut self, leftover: &mut Vec<u8>, reader: &mut R) {
        match self {
            Self::None => {}
            Self::Staged(_) => {
                // Clear the state before committing so a panic cannot cause
                // this action to be committed again.
                let Self::Staged(action) = std::mem::replace(self, Self::None) else {
                    unreachable!();
                };
                action.commit(leftover, reader);
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub(super) enum LeftoverAction {
    #[default]
    None,
    Clear,
}

impl LeftoverAction {
    #[inline]
    fn commit(self, leftover: &mut Vec<u8>) {
        if let Self::Clear = self {
            leftover.clear();
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub(super) struct Action {
    // Bounded by the chunk exposed to the view that recorded this action.
    consume: usize,
    leftover: LeftoverAction,
}

impl Action {
    #[inline]
    pub(super) fn new(consume: usize, leftover: LeftoverAction) -> Self {
        Self { consume, leftover }
    }

    #[inline]
    fn commit<R: BufRead + ?Sized>(self, leftover: &mut Vec<u8>, reader: &mut R) {
        self.leftover.commit(leftover);
        if self.consume > 0 {
            reader.consume(self.consume);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, BufRead, Read};
    use std::panic::{catch_unwind, AssertUnwindSafe};

    use super::{Action, Deferred, LeftoverAction};

    #[test]
    fn commits_staged_action_once() {
        let mut deferred = Deferred::None;
        let mut leftover = b"prefix".to_vec();
        let mut reader = b"abcd".as_slice();

        deferred.commit(&mut leftover, &mut reader);
        assert_eq!(reader, b"abcd");
        assert_eq!(leftover, b"prefix");

        deferred.stage(Action::new(2, LeftoverAction::None));
        deferred.commit(&mut leftover, &mut reader);
        deferred.commit(&mut leftover, &mut reader);
        assert_eq!(reader, b"cd");
        assert_eq!(leftover, b"prefix");

        deferred.stage(Action::new(1, LeftoverAction::Clear));
        deferred.commit(&mut leftover, &mut reader);
        assert_eq!(reader, b"d");
        assert!(leftover.is_empty());
    }

    #[test]
    fn does_not_repeat_action_if_consume_panics() {
        struct PanicOnConsume;

        impl Read for PanicOnConsume {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Ok(0)
            }
        }

        impl BufRead for PanicOnConsume {
            fn fill_buf(&mut self) -> io::Result<&[u8]> {
                Ok(b"x")
            }

            fn consume(&mut self, _: usize) {
                panic!("consume failed");
            }
        }

        let mut deferred = Deferred::None;
        let mut leftover = b"prefix".to_vec();
        let mut reader = PanicOnConsume;
        deferred.stage(Action::new(1, LeftoverAction::Clear));
        assert!(catch_unwind(AssertUnwindSafe(|| {
            deferred.commit(&mut leftover, &mut reader);
        }))
        .is_err());
        assert!(leftover.is_empty());
        assert!(matches!(deferred, Deferred::None));
        deferred.commit(&mut leftover, &mut reader);
    }
}
