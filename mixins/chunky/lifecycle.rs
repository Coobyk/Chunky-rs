// The world's lifecycle: continue the saved task when the world opens, and
// save it (and let it go) when the world closes.
fn chunky_world_lifecycle(
    game: &mut crate::game::Game<'_>,
    phase: crate::game::native_lifecycle::WorldPhase,
) {
    crate::game::chunky_glue::lifecycle(game, phase);
}
