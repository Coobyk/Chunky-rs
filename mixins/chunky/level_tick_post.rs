// After the server level tick pass: pump the workers again while the level
// is still in place.
fn chunky_level_tick_post(game: &mut crate::game::Game<'_>, dimension: &str) {
    crate::game::chunky_glue::level_tick_post(game, dimension);
}
