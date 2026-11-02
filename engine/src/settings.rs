//! The settings page in Options > Mods: a list of buttons, each click moving
//! its setting to the next value (Horizon's native mod settings page).

use crate::Version;
use crate::buf::Uuid;
use crate::denoise::Strength;
use crate::groups::Group;
use crate::voice::{Activation, Config};

/// What a page shows besides the settings themselves.
pub struct PageState<'a> {
    pub mic_test: bool,
    /// The microphone's level now, in dBFS (shown during the test).
    pub level_db: f32,
    pub microphones: &'a [String],
    pub speakers: &'a [String],
    /// On a server's voice chat: the group buttons show.
    pub connected: bool,
    /// The group the player is in, and how many are in it.
    pub group: Option<(&'a Group, usize)>,
    /// The groups to join, with how many are in each.
    pub groups: &'a [(Group, usize)],
}

/// The buttons before the group section.
const SETTINGS: usize = 9;

#[derive(Debug, PartialEq, Eq)]
pub enum Click {
    /// The settings changed: save and apply them.
    Changed,
    ToggleMicTest,
    LeaveGroup,
    CreateGroup,
    /// Join this group (it has a password: ask for it with the command).
    JoinGroup(Uuid, bool),
    Nothing,
}

const THRESHOLDS: [f32; 9] = [-60.0, -55.0, -50.0, -45.0, -40.0, -35.0, -30.0, -25.0, -20.0];
const VOLUMES: [f32; 9] = [0.0, 0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0];
const GAINS: [f32; 7] = [0.5, 0.75, 1.0, 1.5, 2.0, 3.0, 4.0];

/// The value after `now` in `steps`, wrapping (the nearest one when `now` is
/// between steps, as a hand-edited file may have it).
fn next(now: f32, steps: &[f32]) -> f32 {
    let i = steps
        .iter()
        .enumerate()
        .min_by(|a, b| (a.1 - now).abs().total_cmp(&(b.1 - now).abs()))
        .map_or(0, |(i, _)| i);
    steps[(i + 1) % steps.len()]
}

fn next_device(now: &str, names: &[String]) -> String {
    // "" (the system default) comes first.
    let mut all = vec![String::new()];
    all.extend(names.iter().cloned());
    let i = all.iter().position(|n| n == now).unwrap_or(0);
    all[(i + 1) % all.len()].clone()
}

fn device_label(name: &str) -> String {
    if name.is_empty() {
        return "по умолчанию".into();
    }
    // Long Windows names don't fit the button.
    let short: String = name.chars().take(32).collect();
    if short.len() < name.len() { format!("{short}…") } else { short }
}

pub fn labels(c: &Config, page: &PageState<'_>) -> Vec<String> {
    let noise = match c.noise {
        Strength::Off => "выкл",
        Strength::Low => "слабое",
        Strength::Medium => "среднее",
        Strength::High => "сильное",
    };
    vec![
        match c.activation {
            Activation::PushToTalk => "Режим: по кнопке (push-to-talk)".into(),
            Activation::Voice => "Режим: голосовая активация".into(),
        },
        format!("Порог активации: {} дБ", c.threshold_db.round()),
        format!("Шумоподавление: {noise}"),
        format!("Громкость: {}%", (c.volume * 100.0).round()),
        format!("Усиление микрофона: {}%", (c.mic_gain * 100.0).round()),
        format!("Микрофон: {}", device_label(&c.mic_device)),
        format!("Динамики: {}", device_label(&c.speaker_device)),
        if page.mic_test {
            format!("Проверка микрофона: вкл (уровень {} дБ)", page.level_db.round().max(-99.0))
        } else {
            "Проверка микрофона: выкл".into()
        },
        match c.version {
            Version::V20 => "Сервер: Simple Voice Chat 2.6.x".into(),
            Version::V21 => "Сервер: Simple Voice Chat 2.7".into(),
        },
    ]
    .into_iter()
    .chain(group_labels(page))
    .collect()
}

fn group_labels(page: &PageState<'_>) -> Vec<String> {
    if !page.connected {
        return vec!["Группы: зайдите на сервер с голосовым чатом".into()];
    }
    let mut out = vec![
        match page.group {
            Some((g, n)) => format!("Группа: {} ({n}) — выйти", g.name),
            None => "Группа: нет".into(),
        },
        "Создать группу".into(),
    ];
    for (g, n) in page.groups.iter().filter(|(g, _)| Some(g.id) != page.group.map(|(c, _)| c.id)) {
        let lock = if g.has_password { ", пароль" } else { "" };
        out.push(format!("Войти: {} ({n}{lock})", g.name));
    }
    out
}

pub fn click(c: &mut Config, button: usize, page: &PageState<'_>) -> Click {
    match button {
        0 => {
            c.activation = match c.activation {
                Activation::PushToTalk => Activation::Voice,
                Activation::Voice => Activation::PushToTalk,
            }
        }
        1 => c.threshold_db = next(c.threshold_db, &THRESHOLDS),
        2 => {
            let i = Strength::ALL.iter().position(|s| *s == c.noise).unwrap_or(0);
            c.noise = Strength::ALL[(i + 1) % Strength::ALL.len()];
        }
        3 => c.volume = next(c.volume, &VOLUMES),
        4 => c.mic_gain = next(c.mic_gain, &GAINS),
        5 => c.mic_device = next_device(&c.mic_device, page.microphones),
        6 => c.speaker_device = next_device(&c.speaker_device, page.speakers),
        7 => return Click::ToggleMicTest,
        8 => {
            c.version = match c.version {
                Version::V20 => Version::V21,
                Version::V21 => Version::V20,
            }
        }
        _ => return group_click(button - SETTINGS, page),
    }
    Click::Changed
}

fn group_click(i: usize, page: &PageState<'_>) -> Click {
    if !page.connected {
        return Click::Nothing;
    }
    let current = page.group.map(|(g, _)| g.id);
    match i {
        0 if current.is_some() => Click::LeaveGroup,
        0 => Click::Nothing,
        1 => Click::CreateGroup,
        _ => page
            .groups
            .iter()
            .filter(|(g, _)| Some(g.id) != current)
            .nth(i - 2)
            .map_or(Click::Nothing, |(g, _)| Click::JoinGroup(g.id, g.has_password)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page<'a>(mics: &'a [String], spk: &'a [String]) -> PageState<'a> {
        PageState { mic_test: false, level_db: -127.0, microphones: mics, speakers: spk, connected: false, group: None, groups: &[] }
    }

    #[test]
    fn every_button_has_a_label_and_cycles() {
        let mics = vec!["Mic A".to_owned(), "Mic B".to_owned()];
        let none: Vec<String> = Vec::new();
        let p = page(&mics, &none);
        let mut c = Config::default();
        let n = labels(&c, &p).len() - 1;
        assert_eq!(n, SETTINGS);
        for b in 0..n {
            let before = labels(&c, &p)[b].clone();
            let r = click(&mut c, b, &p);
            if b == 7 {
                assert_eq!(r, Click::ToggleMicTest);
            } else if b == 6 {
                // No speakers listed: the default stays.
                assert_eq!(labels(&c, &p)[b], before);
            } else {
                assert_eq!(r, Click::Changed);
                assert_ne!(labels(&c, &p)[b], before, "button {b}");
            }
        }
        assert_eq!(click(&mut c, 99, &p), Click::Nothing);
    }

    #[test]
    fn group_section() {
        use crate::groups::GroupType;
        let g = |id, name: &str, pw| Group { id, name: name.into(), has_password: pw, persistent: false, hidden: false, kind: GroupType::Normal };
        let list = vec![(g(1, "Base", false), 2), (g(2, "Secret", true), 1)];
        let none: Vec<String> = Vec::new();
        let mut p = PageState {
            mic_test: false,
            level_db: -127.0,
            microphones: &none,
            speakers: &none,
            connected: true,
            group: None,
            groups: &list,
        };
        let mut c = Config::default();
        let l = labels(&c, &p);
        assert_eq!(&l[SETTINGS..], ["Группа: нет", "Создать группу", "Войти: Base (2)", "Войти: Secret (1, пароль)"]);
        assert_eq!(click(&mut c, SETTINGS, &p), Click::Nothing);
        assert_eq!(click(&mut c, SETTINGS + 1, &p), Click::CreateGroup);
        assert_eq!(click(&mut c, SETTINGS + 3, &p), Click::JoinGroup(2, true));

        // In Base: it leaves the join list, and the first button leaves it.
        p.group = Some((&list[0].0, 2));
        let l = labels(&c, &p);
        assert_eq!(&l[SETTINGS..], ["Группа: Base (2) — выйти", "Создать группу", "Войти: Secret (1, пароль)"]);
        assert_eq!(click(&mut c, SETTINGS, &p), Click::LeaveGroup);
        assert_eq!(click(&mut c, SETTINGS + 2, &p), Click::JoinGroup(2, true));
    }

    #[test]
    fn devices_cycle_through_default_and_back() {
        let mics = vec!["Mic A".to_owned(), "Mic B".to_owned()];
        let p = page(&mics, &mics);
        let mut c = Config::default();
        for want in ["Mic A", "Mic B", ""] {
            click(&mut c, 5, &p);
            assert_eq!(c.mic_device, want);
        }
    }

    #[test]
    fn steps_wrap_and_snap() {
        assert_eq!(next(-20.0, &THRESHOLDS), -60.0);
        assert_eq!(next(-44.0, &THRESHOLDS), -40.0);
        assert_eq!(next(1.0, &VOLUMES), 1.25);
    }
}
