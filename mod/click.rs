fn voice_click(game: &mut crate::game::Game<'_>, mod_id: &str, button: usize) {
    crate::game::horizon_voice::on_settings_click(game, mod_id, button);
}
