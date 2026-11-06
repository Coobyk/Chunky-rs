// The /chunky commands, as the server's command tree learns them.
fn chunky_register_commands(commands: &mut Vec<crate::game::native_server::NativeCommand>) {
    crate::game::chunky_glue::register(commands);
}
