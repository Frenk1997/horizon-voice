// Horizon Voice: voice chat compatible with Simple Voice Chat servers.
//
// Staged as `crate::game::horizon_voice`, so it reaches the game's own state
// (`remote`, `send`, the native helpers). The network, codec and sound card
// live in the `horizon_voice_engine` crate; this is only the glue: plugin
// messages in and out, the player's position, the keys and the HUD line.

use super::Game;
use super::native_hooks::NativeMod;
use horizon_client::extras::Corner;
use horizon_client::extras_hud::Corners;
use horizon_client::hud::HudBatch;
use horizon_protocol::packets::configuration::CustomPayload;
use horizon_protocol::packets::play::{Clientbound as C, Serverbound as S};
use horizon_voice_engine::audio;
use horizon_voice_engine::groups::GroupType;
use horizon_voice_engine::i18n::Lang;
use horizon_voice_engine::settings::{self, Click, PageState};
use horizon_voice_engine::voice::{Activation, Config, Listener, Status, Voice};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Mutex, MutexGuard, OnceLock};

const MOD_ID: &str = "horizon_voice";
const VERSION: &str = "0.3.2";

struct State {
    voice: Voice,
    // On a server whose Login this session saw.
    active: bool,
    last_status: Status,
    last_muted: bool,
    // The server's real host, being looked up off the game thread.
    resolving: Option<Receiver<String>>,
}

fn state() -> MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE
        .get_or_init(|| {
            let config = match Game::native_config_dir() {
                Ok(dir) => Config::load(&dir),
                Err(_) => Config::default(),
            };
            Mutex::new(State { voice: Voice::new(config), active: false, last_status: Status::Idle, last_muted: false, resolving: None })
        })
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

// The host and port of a server address as typed (`host`, `host:port`,
// `[v6]:port`); 25565 when it names none.
fn split_address(address: &str) -> (String, u16) {
    let a = address.trim();
    if let Some(rest) = a.strip_prefix('[') {
        if let Some((host, tail)) = rest.split_once(']') {
            let port = tail.strip_prefix(':').and_then(|p| p.parse().ok()).unwrap_or(25565);
            return (host.to_owned(), port);
        }
    }
    match a.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => (h.to_owned(), p.parse().unwrap_or(25565)),
        _ => (a.to_owned(), 25565),
    }
}

// Where the game's connection really went: the address's `_minecraft._tcp`
// SRV target, as the game itself connects (many servers' names have no
// address of their own). A DNS query, so it runs on a thread of its own.
fn resolve_host(address: String) -> Receiver<String> {
    let (tx, rx) = mpsc::channel();
    let _ = std::thread::Builder::new().name("horizon-voice-dns".into()).spawn(move || {
        let (host, port) = split_address(&address);
        let (target, _) = horizon_protocol::server_redirect(&host, port);
        let _ = tx.send(target.trim_end_matches('.').to_owned());
    });
    rx
}

pub(super) fn on_packet(game: &mut Game<'_>, packet: &C) {
    match packet {
        C::Login(_) => {
            let Some(address) = game.remote.as_ref().map(|r| r.session.address.clone()) else {
                return;
            };
            let mut st = state();
            st.voice.joined_resolving();
            st.resolving = Some(resolve_host(address));
            st.active = true;
        }
        C::CustomPayload(p) => {
            if p.channel.starts_with("voicechat:") {
                state().voice.payload(&p.channel, &p.data);
            }
        }
        _ => {}
    }
}

fn lang(game: &Game<'_>) -> Lang {
    Lang::of(&game.settings.language)
}

fn flush(game: &Game<'_>, st: &mut State) {
    for (channel, data) in st.voice.take_outgoing() {
        game.send(S::CustomPayload(CustomPayload { channel, data }));
    }
}

pub(super) fn on_frame(game: &mut Game<'_>) {
    if !game.native_window() {
        return;
    }
    let l = lang(game);
    let mut st = state();
    st.voice.lang = l;
    let on_server = game.remote.as_ref().is_some_and(|r| r.joined && r.reconfiguring.is_none());
    if !on_server {
        if st.active {
            st.voice.left();
            st.active = false;
            st.resolving = None;
            st.last_status = Status::Idle;
        }
        // The microphone test works from the title screen's Options too.
        st.voice.idle_frame();
        return;
    }
    if let Some(rx) = &st.resolving {
        if let Ok(host) = rx.try_recv() {
            st.resolving = None;
            st.voice.set_host(&host);
        }
    }
    if !st.active {
        return;
    }
    flush(game, &mut st);
    let Some(p) = game.native_player() else { return };
    let listener = Listener { position: [p.x, p.y, p.z], yaw: p.yaw };
    {
        let remote = game.remote.as_ref();
        let position_of = |uuid: u128| -> Option<[f64; 3]> {
            let r = remote?;
            let id = r.player_entity(uuid)?;
            r.tracked(id as i32).map(|(_, at)| at)
        };
        st.voice.frame(listener, &position_of);
    }
    flush(game, &mut st);
    let notices = st.voice.take_notices();
    if !notices.is_empty() {
        drop(st);
        for n in notices {
            game.native_chat(&format!("[Voice] {n}"));
        }
        st = state();
    }

    let status = st.voice.status().clone();
    if status != st.last_status {
        let line = match &status {
            Status::Connected => {
                let key = push_to_talk_key(game, &st);
                let how = match st.voice.config.activation {
                    Activation::PushToTalk => {
                        let k = key_label(&key);
                        l.pick(format!("hold {k} to talk"), format!("держи {k}, чтобы говорить"))
                    }
                    Activation::Voice => l.pick("voice activation", "голосовая активация").to_owned(),
                };
                let mut line = l.pick(format!("[Voice] Connected ({how})"), format!("[Voice] Подключено ({how})"));
                if let Some(e) = st.voice.audio_error() {
                    line.push_str(&l.pick(format!(" - audio problem: {e}"), format!(" - проблема со звуком: {e}")));
                }
                Some(line)
            }
            Status::Failed(e) => Some(l.pick(format!("[Voice] Not connected: {e}"), format!("[Voice] Нет подключения: {e}"))),
            Status::Connecting | Status::Idle => None,
        };
        st.last_status = status;
        drop(st);
        if let Some(line) = line {
            game.native_chat(&line);
        }
    }
}

fn push_to_talk_key(game: &Game<'_>, st: &State) -> String {
    game.native_mod_key(MOD_ID, "push_to_talk", &st.voice.config.ptt_key)
}

// `key.keyboard.v` → `V`.
fn key_label(key: &str) -> String {
    key.rsplit('.').next().unwrap_or(key).to_uppercase()
}

pub(super) fn on_key(game: &mut Game<'_>, key: &str, pressed: bool) {
    let mut st = state();
    let ptt = push_to_talk_key(game, &st);
    let mute = game.native_mod_key(MOD_ID, "mute", &st.voice.config.mute_key);
    let menu = game.native_mod_key(MOD_ID, "menu", MENU_KEY);
    if key == menu && pressed && game.native_playing() {
        drop(st);
        open_menu(game);
    } else if key == ptt {
        // Not while typing in chat or in a menu; a release always counts.
        st.voice.set_push_to_talk(pressed && game.native_playing());
    } else if key == mute && pressed && game.native_playing() && st.active {
        st.voice.toggle_mute();
        let muted = st.voice.muted();
        st.last_muted = muted;
        drop(st);
        let l = lang(game);
        game.native_chat(if muted {
            l.pick("[Voice] Microphone muted", "[Voice] Микрофон выключен")
        } else {
            l.pick("[Voice] Microphone on", "[Voice] Микрофон включён")
        });
    }
}

const MENU_KEY: &str = "key.keyboard.g";

// The mod's page in Options > Mods, straight from the game, as the control
// socket's `options` command opens a page.
fn open_menu(game: &mut Game<'_>) {
    use horizon_client::options_ui::Page;
    game.capture(false);
    game.open_options(false);
    game.ui.options.show(Page::Mods);
    game.list_native_mods();
    if let Some(i) = game.settings.mods.installed.iter().position(|m| m.id == MOD_ID) {
        game.ui.options.show(Page::NativeMod(i));
        game.native_settings_refresh(i);
    }
}

// `/vgroup ...`: the groups, with names and passwords the page can't type.
const USAGE_EN: &str = "/vgroup list | join <group> [password] | create <group> [password] [normal|open|isolated] | leave";
const USAGE_RU: &str = "/vgroup list | join <группа> [пароль] | create <группа> [пароль] [normal|open|isolated] | leave";

// Words, with "quoted phrases" as one.
fn words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for ch in text.chars() {
        match ch {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

pub(super) fn on_commands(commands: &mut Vec<&'static str>) {
    commands.push("vgroup");
}

pub(super) fn on_command(game: &mut Game<'_>, command: &str, ran: &mut bool) {
    let w = words(command);
    if w.first().map(String::as_str) != Some("vgroup") {
        return;
    }
    *ran = true;
    let l = lang(game);
    let reply = {
        let mut st = state();
        if !st.voice.connected() {
            l.pick("Voice chat is not connected", "Голосовой чат не подключён").to_owned()
        } else {
            match (w.get(1).map(String::as_str), w.get(2)) {
                (Some("list") | None, _) => {
                    let list = st.voice.group_list();
                    if list.is_empty() {
                        l.pick("No groups. Create one: /vgroup create <name>", "Групп нет. Создать: /vgroup create <название>").to_owned()
                    } else {
                        let current = st.voice.current_group().map(|g| g.id);
                        let lines: Vec<String> = list
                            .iter()
                            .map(|(g, n)| {
                                let lock = if g.has_password { l.pick(", password", ", пароль") } else { "" };
                                let here = if Some(g.id) == current { l.pick(" <- you are here", " <- вы здесь") } else { "" };
                                let who = st.voice.members(g.id).join(", ");
                                format!("{} ({n}{lock}): {who}{here}", g.name)
                            })
                            .collect();
                        format!("{}: {}", l.pick("Groups", "Группы"), lines.join("; "))
                    }
                }
                (Some("join"), Some(name)) => match st.voice.find_group(name).map(|g| g.id) {
                    Some(id) => {
                        st.voice.join_group(id, w.get(3).map(String::as_str));
                        l.pick(format!("Joining group \"{name}\"..."), format!("Вход в группу «{name}»…"))
                    }
                    None => l.pick(format!("No group \"{name}\": /vgroup list"), format!("Нет группы «{name}»: /vgroup list")),
                },
                (Some("create"), Some(name)) => {
                    let kind = match w.get(4).map(String::as_str) {
                        Some("open") => GroupType::Open,
                        Some("isolated") => GroupType::Isolated,
                        _ => GroupType::Normal,
                    };
                    st.voice.create_group(name, w.get(3).map(String::as_str), kind);
                    l.pick(format!("Creating group \"{name}\"..."), format!("Создаю группу «{name}»…"))
                }
                (Some("leave"), _) => {
                    st.voice.leave_group();
                    l.pick("Leaving the group...", "Выход из группы…").to_owned()
                }
                _ => l.pick(USAGE_EN, USAGE_RU).to_owned(),
            }
        }
    };
    game.native_chat(&format!("[Voice] {reply}"));
}

pub(super) fn on_hud(game: &mut Game<'_>, batch: &mut HudBatch, corners: &mut Corners) {
    let l = lang(game);
    let st = state();
    if !(st.active || st.voice.mic_test()) || !game.native_corner_shown(Corner::BottomLeft) {
        return;
    }
    let speakers = st.voice.speakers().len();
    let word = l.pick("Voice", "Голос");
    let voice = match st.voice.current_group() {
        Some(g) => format!("{word} [{}]", g.name),
        None => word.to_owned(),
    };
    let (text, rgb) = match st.voice.status() {
        _ if st.voice.mic_test() => {
            let rgb = if st.voice.talking() { 0x55FF55 } else { 0xAAAAAA };
            let level = st.voice.level_db().round().max(-99.0);
            (l.pick(format!("Voice: mic test {level} dB"), format!("Голос: проверка микрофона {level} дБ")), rgb)
        }
        Status::Connected if st.voice.muted() => (format!("{voice}: {}", l.pick("muted", "микрофон выкл")), 0xFF5555),
        Status::Connected if st.voice.talking() => (format!("{voice}: {}", l.pick("talking", "говоришь")), 0x55FF55),
        Status::Connected if speakers > 0 => (l.pick(format!("{voice}: {speakers} speaking"), format!("{voice}: говорят {speakers}")), 0xFFFF55),
        Status::Connected => (voice, 0xAAAAAA),
        Status::Connecting => (format!("{word}: {}", l.pick("connecting", "подключение")), 0xAAAAAA),
        Status::Failed(_) => (format!("{word}: {}", l.pick("off", "выкл")), 0xFF5555),
        Status::Idle => return,
    };
    drop(st);
    let w = game.native_text_width(&text) + 4.0;
    let h = 12.0;
    let (x, y) = corners.place(Corner::BottomLeft, w, h);
    game.native_fill(batch, [x, y, w, h], 0x78000000);
    game.native_text(batch, &text, [x + 2.0, y + 2.0], rgb, true);
}

// Options > Mods: the mod's row, its keys (rebindable there) and its page.
pub(super) fn on_mods(game: &Game<'_>, mods: &mut Vec<NativeMod>) {
    let l = lang(game);
    let key = |name: &str, label: &str, default: &str| [name.to_owned(), label.to_owned(), default.to_owned()];
    mods.push(NativeMod {
        id: MOD_ID.into(),
        title: format!("Horizon Voice {VERSION}"),
        description: l.pick("Voice chat compatible with Simple Voice Chat", "Голосовой чат, совместимый с Simple Voice Chat").into(),
        keys: vec![
            key("push_to_talk", l.pick("Push to talk", "Говорить (push-to-talk)"), "key.keyboard.v"),
            key("mute", l.pick("Mute microphone", "Выключить микрофон"), "key.keyboard.m"),
            key("menu", l.pick("Voice chat menu", "Меню голосового чата"), MENU_KEY),
        ],
    });
}

fn page_labels(st: &State, lang: Lang) -> Vec<String> {
    let microphones = audio::input_device_names();
    let speakers = audio::output_device_names();
    let groups = st.voice.group_list();
    let current = st.voice.current_group().map(|g| (g, st.voice.members(g.id).len()));
    let page = PageState {
        lang,
        mic_test: st.voice.mic_test(),
        level_db: st.voice.level_db(),
        microphones: &microphones,
        speakers: &speakers,
        connected: st.voice.connected(),
        group: current,
        groups: &groups,
    };
    settings::labels(&st.voice.config, &page)
}

pub(super) fn on_settings(game: &Game<'_>, mod_id: &str, buttons: &mut Vec<String>) {
    if mod_id != MOD_ID {
        return;
    }
    buttons.extend(page_labels(&state(), lang(game)));
}

pub(super) fn on_settings_click(game: &mut Game<'_>, mod_id: &str, button: usize) {
    if mod_id != MOD_ID {
        return;
    }
    let l = lang(game);
    let mut st = state();
    let microphones = audio::input_device_names();
    let speakers = audio::output_device_names();
    let groups = st.voice.group_list();
    let current = st.voice.current_group().cloned();
    let current_n = current.as_ref().map_or(0, |g| st.voice.members(g.id).len());
    let page = PageState {
        lang: l,
        mic_test: st.voice.mic_test(),
        level_db: st.voice.level_db(),
        microphones: &microphones,
        speakers: &speakers,
        connected: st.voice.connected(),
        group: current.as_ref().map(|g| (g, current_n)),
        groups: &groups,
    };
    let mut config = st.voice.config.clone();
    let mut say = None;
    match settings::click(&mut config, button, &page) {
        Click::LeaveGroup => st.voice.leave_group(),
        Click::CreateGroup => {
            let name = match st.voice.my_name() {
                Some(me) => l.pick(format!("{me}'s group"), format!("Группа {me}")),
                None => l.pick("My group", "Моя группа").to_owned(),
            };
            st.voice.create_group(&name, None, GroupType::Normal);
        }
        Click::JoinGroup(id, false) => st.voice.join_group(id, None),
        Click::JoinGroup(id, true) => {
            let name = groups.iter().find(|(g, _)| g.id == id).map_or(String::new(), |(g, _)| g.name.clone());
            say = Some(l.pick(
                format!("[Voice] This group has a password: /vgroup join \"{name}\" <password>"),
                format!("[Voice] У группы пароль: /vgroup join \"{name}\" <пароль>"),
            ));
        }
        Click::Changed => {
            if let Ok(dir) = Game::native_config_dir() {
                let _ = config.save(&dir);
            }
            st.voice.apply_config(config);
        }
        Click::ToggleMicTest => {
            let on = !st.voice.mic_test();
            st.voice.set_mic_test(on);
        }
        Click::Nothing => {}
    }
    drop(st);
    if let Some(line) = say {
        game.native_chat(&line);
    }
}
