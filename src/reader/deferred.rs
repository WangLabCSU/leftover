use std::io::BufRead;

#[derive(Debug, Default, PartialEq, Eq, Hash)]
pub(super) enum Deferred {
    #[default]
    None,
    Staged(Action),
}

impl Deferred {
    // Commit the previous action before granting exclusive access to stage
    // the next one. Dropping the returned permission leaves this slot empty.
    #[inline]
    pub(super) fn commit<R: BufRead + ?Sized>(
        &mut self,
        leftover: &mut Vec<u8>,
        reader: &mut R,
    ) -> Staging<'_> {
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
        Staging { slot: self }
    }
}

/// Exclusive permission to stage one action in an empty deferred slot.
///
/// Staging requires ownership, so a view cannot stage an action through
/// `&mut self` while remaining available for further operations.
///
/// ```compile_fail,E0507
/// # mod reader {
/// #     mod deferred {
/// #         include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/reader/deferred.rs"));
/// #     }
/// #
/// #     use self::deferred::{Action, Staging};
/// fn stage_from_borrow(staging: &mut Staging<'_>) {
///     staging.stage(Action::default());
/// }
/// # }
/// ```
///
/// The permission cannot be reused to replace an action already staged.
///
/// ```compile_fail,E0382
/// # mod reader {
/// #     mod deferred {
/// #         include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/reader/deferred.rs"));
/// #     }
/// #
/// #     use self::deferred::{Action, Staging};
/// fn stage_twice(mut staging: Staging<'_>) {
///     staging.stage(Action::default());
///     staging.stage(Action::default());
/// }
/// # }
/// ```
#[derive(Debug)]
pub(super) struct Staging<'a> {
    // Only Deferred::commit constructs this value, after clearing the slot.
    // Keep the slot private: exposing it would allow staging through a borrow.
    slot: &'a mut Deferred,
}

impl Staging<'_> {
    #[inline]
    pub(super) fn stage(self, action: Action) {
        *self.slot = Deferred::Staged(action);
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

        let staging = deferred.commit(&mut leftover, &mut reader);
        assert_eq!(reader, b"abcd");
        assert_eq!(leftover, b"prefix");

        staging.stage(Action::new(2, LeftoverAction::None));
        deferred.commit(&mut leftover, &mut reader);
        deferred.commit(&mut leftover, &mut reader);
        assert_eq!(reader, b"cd");
        assert_eq!(leftover, b"prefix");

        deferred
            .commit(&mut leftover, &mut reader)
            .stage(Action::new(1, LeftoverAction::Clear));
        deferred.commit(&mut leftover, &mut reader);
        assert_eq!(reader, b"d");
        assert!(leftover.is_empty());
    }

    #[test]
    fn commits_previous_action_before_staging_the_next() {
        let mut deferred = Deferred::None;
        let mut leftover = b"prefix".to_vec();
        let mut reader = b"abcd".as_slice();

        deferred
            .commit(&mut leftover, &mut reader)
            .stage(Action::new(1, LeftoverAction::Clear));

        let staging = deferred.commit(&mut leftover, &mut reader);
        assert!(leftover.is_empty());
        assert_eq!(reader, b"bcd");
        staging.stage(Action::new(2, LeftoverAction::None));

        deferred.commit(&mut leftover, &mut reader);
        assert_eq!(reader, b"d");
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
        deferred
            .commit(&mut leftover, &mut reader)
            .stage(Action::new(1, LeftoverAction::Clear));
        assert!(catch_unwind(AssertUnwindSafe(|| {
            deferred.commit(&mut leftover, &mut reader);
        }))
        .is_err());
        assert!(leftover.is_empty());
        assert!(matches!(deferred, Deferred::None));
        deferred.commit(&mut leftover, &mut reader);
    }
}
