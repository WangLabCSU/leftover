use std::io::BufRead;

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
    pub(super) fn commit<R: BufRead + ?Sized>(self, leftover: &mut Vec<u8>, reader: &mut R) {
        self.leftover.commit(leftover);
        if self.consume > 0 {
            reader.consume(self.consume);
        }
    }
}
