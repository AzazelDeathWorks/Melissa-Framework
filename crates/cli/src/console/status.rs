use super::chat::Conversation;
use crate::{process_measurement::Measurement, settings::Settings};
use melissa_model_gateway::local_tokenizer::WireTokenMeasurement;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContextSnapshot {
    pub native_body_bytes: u64,
    pub capacity_tokens: u64,
    pub reserved_output_tokens: u64,
    pub provider_input_tokens: Option<u64>,
    pub wire_tokens: Option<WireTokenMeasurement>,
    pub wire_tokenizer_status: String,
    pub summarizing: bool,
}

#[derive(Default)]
pub(crate) struct ModelLabel {
    checked_at: Option<Instant>,
    value: Option<String>,
}
impl ModelLabel {
    pub fn get(&mut self, now: Instant, read: impl FnOnce() -> Option<String>) -> &str {
        if self
            .checked_at
            .is_none_or(|t| now.saturating_duration_since(t) >= Duration::from_secs(1))
        {
            self.value = read();
            self.checked_at = Some(now);
        }
        self.value.as_deref().unwrap_or("model seçilmedi")
    }
    pub fn invalidate(&mut self) {
        self.checked_at = None;
    }
}

fn count(value: Option<u64>) -> String {
    value
        .map(|n| n.to_string())
        .unwrap_or_else(|| "bilinmiyor".into())
}
pub(crate) fn report(
    settings: Option<&Settings>,
    conversation: Option<&Conversation>,
    active: bool,
    plan: bool,
    queued: usize,
    resources: &Measurement,
) -> String {
    let mut lines = vec![
        format!(
            "Durum: {}",
            if active {
                "çalışıyor"
            } else if plan {
                "PLAN · uygulama kapalı"
            } else {
                "hazır"
            }
        ),
        format!("Bekleyen mesaj: {queued}"),
        format!(
            "Model: {}",
            settings.map(|s| s.model.as_str()).unwrap_or("seçilmedi")
        ),
    ];
    if let Some(s) = settings {
        lines.push(format!(
            "Bağlam kapasitesi: {} token · çıktı rezervi: {} token",
            s.context_tokens, s.max_output_tokens
        ));
    }
    if let Some(c) = conversation {
        lines.push(format!(
            "Belirsiz model çağrısı: {}",
            if active {
                "etkin işin sonucu bekleniyor; aşağıdakiler önceki kayıtlar"
            } else if c.pending.is_some() {
                "var; otomatik tekrar yok"
            } else {
                "son kayıtta yok"
            }
        ));
        if let Some(context) = &c.last_context {
            lines.push(format!(
                "Son kaydedilen istek{}: {} bayt native JSON",
                if context.summarizing { " (özet)" } else { "" },
                context.native_body_bytes
            ));
            lines.push(format!(
                "Bu isteğe ait sağlayıcı girdi sayımı: {} token",
                count(context.provider_input_tokens)
            ));
            lines.push(format!(
                "Bu istekteki kapasite: {} token · çıktı rezervi: {} token",
                context.capacity_tokens, context.reserved_output_tokens
            ));
            lines.push(format!(
                "Yerel JSON metni sayımı: {} token ({})",
                count(context.wire_tokens.as_ref().map(|t| t.token_count)),
                context.wire_tokenizer_status
            ));
        }
    }
    lines.push("Bayt sınırı token ölçümü değildir. Yerel JSON tokenizer'ı sağlayıcının chat şablonunu/görsel hesabını ölçmez; kapasite veya maliyet yetkisi vermez. Son yanıtın sayımı bir sonraki isteğin tahmini değildir.".into());
    lines.push(format!(
        "CLI RSS: {} bayt · süreç ömrü tepe RSS: {} bayt",
        count(resources.latest.working_set_bytes),
        count(resources.latest.process_lifetime_peak_working_set_bytes)
    ));
    lines.push(format!("Başlatma sonrası tepe RSS: {} bayt (destekleniyorsa). Süreç ömrü tepesi exec öncesi başlatıcı belleğini içerebilir.", count(resources.latest.post_exec_peak_working_set_bytes)));
    lines.push(format!(
        "Bu oturumdaki CLI CPU (user+system): {} µs",
        count(resources.cpu_time_delta_us)
    ));
    lines.push("Yalnız bu CLI süreci; worker/alt süreçler dahil değil. CPU yüzdesi veya fiziksel disk ölçümü değildir. Bu ekran model çağırmaz ve yeni yetki vermez.".into());
    lines.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn redraws_do_not_reread_settings_until_expiry_or_invalidation() {
        let mut cache = ModelLabel::default();
        let start = Instant::now();
        assert_eq!(cache.get(start, || Some("first".into())), "first");
        for ms in 1..1000 {
            assert_eq!(
                cache.get(start + Duration::from_millis(ms), || panic!(
                    "frame read settings"
                )),
                "first"
            );
        }
        assert_eq!(
            cache.get(start + Duration::from_secs(1), || Some(
                "external update".into()
            )),
            "external update"
        );
        cache.invalidate();
        assert_eq!(
            cache.get(start + Duration::from_secs(1), || None),
            "model seçilmedi"
        );
    }
    #[test]
    fn status_distinguishes_unknown_counts_from_measured_zero() {
        assert_eq!(count(None), "bilinmiyor");
        assert_eq!(count(Some(0)), "0");
        let baseline = crate::process_measurement::sample();
        let report = report(None, None, true, true, 2, &Measurement::observe(&baseline));
        assert!(report.contains("çalışıyor"));
        assert!(report.contains("Bekleyen mesaj: 2"));
        assert!(report.contains("Bayt sınırı token ölçümü değildir"));
        assert!(report.contains("worker/alt süreçler dahil değil"));
    }
}
