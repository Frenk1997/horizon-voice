fn voice_page(game: &crate::game::Game<'_>, mod_id: &str, buttons: &mut Vec<String>) {
    crate::game::horizon_voice::on_settings(game, mod_id, buttons);
}
