fn voice_command(game: &mut crate::game::Game<'_>, command: &str, ran: &mut bool) {
    crate::game::horizon_voice::on_command(game, command, ran);
}
