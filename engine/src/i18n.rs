//! The mod's two languages: Russian for a game in Russian, else English.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lang {
    #[default]
    En,
    Ru,
}

impl Lang {
    /// From the game's language code (`en_us`, `ru_ru`).
    pub fn of(code: &str) -> Lang {
        if code.to_lowercase().starts_with("ru") { Lang::Ru } else { Lang::En }
    }

    pub fn pick<T>(self, en: T, ru: T) -> T {
        match self {
            Lang::En => en,
            Lang::Ru => ru,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn russian_only_for_a_russian_game() {
        assert_eq!(Lang::of("ru_ru"), Lang::Ru);
        assert_eq!(Lang::of("RU_RU"), Lang::Ru);
        for code in ["en_us", "de_de", "uk_ua", ""] {
            assert_eq!(Lang::of(code), Lang::En, "{code}");
        }
        assert_eq!(Lang::Ru.pick("a", "b"), "b");
    }
}
