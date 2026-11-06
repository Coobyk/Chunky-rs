// A player's /chunky command on the server: Chunky's, when it is one.
fn chunky_perform_command(
    game: &mut crate::game::Game<'_>,
    player: &crate::game::native_server::NativeServerPlayer,
    command: &str,
    ran: &mut bool,
) {
    crate::game::chunky_glue::perform(game, Some(player), command, ran);
}
