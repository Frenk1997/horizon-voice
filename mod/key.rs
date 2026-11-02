fn voice_key(game: &mut crate::game::Game<'_>, key: &str, pressed: bool) {
    crate::game::horizon_voice::on_key(game, key, pressed);
}
