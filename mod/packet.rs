fn voice_packet(game: &mut crate::game::Game<'_>, packet: &horizon_protocol::packets::play::Clientbound) {
    crate::game::horizon_voice::on_packet(game, packet);
}
