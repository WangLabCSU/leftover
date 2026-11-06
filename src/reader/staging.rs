use super::action::Action;

/// Exclusive permission to stage one action in an empty deferred slot.
///
/// Staging consumes the permission, so it cannot be used through a mutable
/// borrow or reused to replace an action already staged.
///
/// ```compile_fail,E0507
/// # mod reader {
/// #     include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/reader/mod.rs"));
/// fn stage_from_borrow(staging: &mut staging::Staging<'_>) {
///     staging.stage(action::Action::default());
/// }
/// # }
/// ```
///
/// ```compile_fail,E0382
/// # mod reader {
/// #     include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/reader/mod.rs"));
/// fn stage_twice(staging: staging::Staging<'_>) {
///     staging.stage(action::Action::default());
///     staging.stage(action::Action::default());
/// }
/// # }
/// ```
#[derive(Debug)]
pub(super) struct Staging<'a> {
    // Keep the slot private so view methods must consume this permission to
    // stage an action, rather than writing through a borrowed slot.
    slot: &'a mut Option<Action>,
}

impl<'a> Staging<'a> {
    /// Create permission to stage an action after the previous one is committed.
    #[inline]
    pub(super) fn new(slot: &'a mut Option<Action>) -> Self {
        debug_assert!(slot.is_none(), "staging requires an empty deferred slot");
        Self { slot }
    }

    #[inline]
    pub(super) fn stage(self, action: Action) {
        *self.slot = Some(action);
    }
}
