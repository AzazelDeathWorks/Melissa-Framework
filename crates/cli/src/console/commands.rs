pub(super) const COMMANDS: &[(&str, &str)] = &[
    ("/help", "Komutlar ve klavye kısayolları"),
    ("/status", "Bağlam, bütçe ve kaynaklar · API çağırmaz"),
    ("/settings", "Sağlayıcı, model ve tercihler"),
    ("/model", "Konuşmayı koruyarak model değiştir"),
    ("/project", "Çalışma klasörü bağla"),
    ("/plan", "Salt okunur araştırma ve planlama"),
    ("/apply", "Planı kesin kapsam incelemesine al"),
    ("/new", "Önceki kaydı koruyarak yeni konuşma"),
    ("/resume", "Kayıtlı konuşmaya dön"),
    ("/diff", "Değişiklikleri incele"),
    ("/undo", "Seçilen değişikliği kontrollü geri al"),
    ("/compact", "Özgün geçmişi koruyarak bağlamı özetle"),
    ("/search", "Konuşmada ara"),
    ("/transcript", "Konuşma ve araç ayrıntıları"),
    ("/queue", "Bekleyen mesajları incele"),
    ("/exit", "Güvenli duruş ve çıkış"),
];

pub(super) fn matching(prefix: &str) -> Vec<(&'static str, &'static str)> {
    if !prefix.starts_with('/') || prefix.contains(char::is_whitespace) {
        return Vec::new();
    }
    let prefix = if prefix == "/config" {
        "/settings"
    } else {
        prefix
    };
    COMMANDS
        .iter()
        .copied()
        .filter(|(name, _)| name.starts_with(prefix))
        .collect()
}

pub(super) fn help() -> String {
    let mut text = COMMANDS
        .iter()
        .map(|(name, description)| format!("{name:<14} {description}"))
        .collect::<Vec<_>>()
        .join("\n");
    text.push_str("\n\nCtrl+P komut paleti · Tab tamamla · Enter gönder\nAlt+Enter yeni satır · Shift+Tab plan · Esc durdur\nCtrl+F ara · F3 sonraki · Ctrl+O araç ayrıntıları\nPgUp/PgDn geçmiş · Ctrl+End son mesaj · Ctrl+R mesaj geçmişi\nCtrl+Q sonraki işe bırak · /config, /settings eşanlamlıdır.\n\nKomut seçmek yalnız taslağı doldurur; çalıştırmak için Enter gerekir.\nPlan modu yazma veya çalıştırma yetkisi vermez.\nTeknik otomasyon: melissa --help-all");
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn completion_and_help_share_every_command_without_effects() {
        assert_eq!(matching("/").len(), COMMANDS.len());
        assert!(matching("/st").iter().any(|(name, _)| *name == "/status"));
        assert_eq!(matching("/config"), matching("/settings"));
        assert!(matching("hello").is_empty());
        assert!(matching("/plan write").is_empty());
        for (command, _) in COMMANDS {
            assert!(help().contains(command));
        }
    }
}
