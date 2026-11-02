fn voice_commands(commands: &mut Vec<&'static str>) {
    crate::game::horizon_voice::on_commands(commands);
}
