// The row in Options > Mods.
fn chunky_list(mods: &mut Vec<crate::game::native_hooks::NativeMod>) {
    crate::game::chunky_glue::list(mods);
}
