// A /chunky command typed on the dedicated server's console: Chunky's, when
// it is one.
fn chunky_perform_console_command(
    game: &mut crate::game::Game<'_>,
    command: &str,
    ran: &mut bool,
) {
    crate::game::chunky_glue::perform(game, None, command, ran);
}
