fn voice_listed(game: &crate::game::Game<'_>, mods: &mut Vec<crate::game::native_hooks::NativeMod>) {
    crate::game::horizon_voice::on_mods(game, mods);
}
