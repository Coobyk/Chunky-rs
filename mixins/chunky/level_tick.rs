// The task driver, around each server level tick pass: it moves the band
// past the lit rows, hands it to the level's streaming, and reports.
fn chunky_level_tick(game: &mut crate::game::Game<'_>, dimension: &str) {
    crate::game::chunky_glue::level_tick(game, dimension);
}
