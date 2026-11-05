# Harness Lab

Laboratuvar bir değişikliğin katkısını aynı kaynak, model ayarı, görev bütçesi
ve korumalı kabul tarifine sahip iki kolda ölçer. Her deneme yeni bir çalışma
klasörü ve şifreli görev deposu açar. Dosya kapsamı, korumalı kabul ve kaynak
kontrolleri deney sırasında da uygulanır.

## Bir deney çalıştırmak

Özel planı, sağlayıcı ayarlarını, beklenen cevapları ve çıktı klasörünü
yayımlanacak deponun dışında tut. Üretim deposunun dışarı aktarılmış bir
kopyasını veya özel bir test projesini kullan. Önceden hazırlanmış, farklı
bir sonuç üreten negatif kontrol ağacını ayrıca ver.

```json
{
  "schema_version": "melissa.lab-plan/v1",
  "campaign_kind": "pilot",
  "axis": "context_state",
  "worker_image": "sha256:<melissa doctor tarafından bildirilen tam imaj kimliği>",
  "order_seeds": [19, 31],
  "max_campaign_micro_usd": 8000000,
  "arms": {
    "baseline": { "binary": "melissa.exe", "settings": "control.private.json" },
    "treatment": { "binary": "melissa.exe", "settings": "standard.private.json" }
  },
  "cases": [{
    "id": "pagination",
    "cohort": "small",
    "goal": "Fix pagination and verify the public checks.",
    "source": "fixture/source",
    "negative": "fixture/negative",
    "recipe": "fixture/recipe.private.json"
  }]
}
```

Bu örnekte kontrol ayarına `"lab_variant": "context_state_off"` ekle.
Standart ayar dosyası aynı model, kapasite, fiyat kartı ve diğer ayarları
kullanır. Tarif mevcut izole program SDK tarifidir. Tarifteki negatif çalışma
yolu her denemenin kendi doğrulanmış kopyasına bağlanır.

```text
python tools/harness_lab.py freeze PRIVATE_PLAN NEW_FROZEN_PLAN
python tools/harness_lab.py run FROZEN_PLAN NEW_OUTPUT_DIRECTORY --allow-live-micro-usd 8000000
```

`--jobs 4` dört bağımsız denemeyi aynı dalgada açar. Her denemenin uzmanları
kendi görevinde seri çalışır. Koşucu tüm dalganın görev bütçesini önceden
ayırır. Disk alanı veya bütçe daraldığında dalga küçülür. Rapor, denemeleri
eşzamanlılık düzeyiyle etiketler ve farklı yüklerdeki süreleri tek ortalamada
birleştirmez.

`freeze` kaynak, negatif kontrol, tarif, ikili dosya, ayar ve ortam
kimliklerini kaydeder. `run` toplam harcama tavanını denetler. Sonraki
denemeyi açmadan önce sonuçlanan görevlerin muhasebesi ile sıradaki görev
bütçesini toplar. Sağlayıcının kesin faturası ayrıca doğrulanır.
Windows’ta kısa bir çıktı yolu kullan. Vaka kimlikleri raporda tutulur,
görev deposu yolunu uzatmaz.
Deneme başlamadan önce çıktı diskinde en az 4 GiB boş alan aranır.
Disk sınırında durursa `--resume` önceki duruş raporunu koruyarak devam eder.
Sıralama tohumları deneme sırasını belirler. Model örneklemesinin sağlayıcı
tarafında sabitlendiğine dair kanıt oluşturmazlar.

## Deney eksenleri

| Eksen | Kontrol kolu | Kapsam |
|---|---|---|
| `context_state` | `context_state_off` | Her model turuna eklenen güncel görev durumunu ayırır. |
| `compaction` | `compaction_off` | Gözlem normalizasyonu, arşivleme ve dönüm noktası sıkıştırmasını ayırır. |
| `repository_index_cache` | `repository_index_cache_off` | Görevler arası kalıcı indeks önbelleğini ayırır. İndeksli arama korunur. |
| `project_memory` | `project_memory_off` | Proje belleği araçlarını ayırır. Yarar ölçümü için yetkili, kaynakla eşleşen önceki bellek gerekir. |
| `adaptive_replanning` | `adaptive_replanning_off` | Modelin plan önerisi aracını ayırır. Seri uzman sırası ve korumalı QA uygulanır. |
| `tool_profile` | Seçilen araç profili | İki etkili araç profilini aynı modelle karşılaştırır. |
| `model_routing` | Birincil model | Aynı birincil ayarla alternatif model havuzunun etkisini ölçer. |
| `runtime_revision` | Ayrı, sabit ikili dosya | Birden çok değişiklik içerebilen iki sürümü karşılaştırır. İki ikili de Lab ölçüm sözleşmesini destekler. |

Yalnız tek bir eksen değişebilir. `full` kod değerlendirmesi en az 50 küçük,
50 depo, 30 uzun ve 20 büyük Rust çalışma alanı vakası gerektirir. Kesinti,
kurtarma ve güvenlik regresyonları ayrıca çalıştırılır.

## Sonuçları yorumlamak

`trial.json` her denemenin kabul ve muhasebe durumunu kaydeder.
`summary.json` tamamlanan kollar için doğrulanmış başarı oranı ve başarı
başına maliyet, token ve süreyi verir. Başarısız denemelerin süre ve bilinen
kullanımı toplama girer. Eksik sağlayıcı verisi ilgili karşılaştırmayı açık
bırakır. Sonucu belirsiz bir dış çağrı kampanyayı durdurur ve kayıt korunur.
`--resume` yalnız kesintiden önceki denemelerin tamamı sonuçlanmış ve
ara raporları kaydedilmişse çalışır. Çözümlenmemiş bir deneme yeniden
gönderilmez.
`--continue-after-inconclusive` ayrı vakalara devam etmeyi etkinleştirir.
Önceki denemenin yürütücüsü ve worker’ları durmuş olmalı. Belirsiz dış çağrı
için ayrılan tutar bütçeden düşülür ve önceki denemenin durumu korunur.
`--resume --raise-campaign-cap --allow-live-micro-usd NEW_CAP` açıkça
yetkilendirilmiş bir artışı duruş raporuna bağlar. Kaynak, model ayarları,
deneme sırası ve önceki harcama kayıtları aynı kampanya içinde tutulur.

`python tools/harness_lab.py review FROZEN_PLAN RESULTS_DIRECTORY NEW_REPORT`
yalnız kayıtlı denemelerden bağımsız inceleme özeti çıkarır. İşlem sırasında
yarım kalan klasörleri işaretler ve model çağrısı başlatmaz.

Yerel sentetik set `python tools/harness_lab_fixtures.py PRIVATE_DIRECTORY
--seed 12345 --max-case-micro-usd 2000000` ile oluşturulabilir. Üretilen
`cases.private.json` dosyasındaki vakalar aynı dizine konan planın
`cases` alanına eklenir. Bu set kaynak boyutu, çok dosyalı değişiklik ve
korumalı kabul senaryolarını kapsar. Daha geniş kodlama kalitesi için
bağımsız depolar ve görevler ayrı grupta ölçülür.

Windows süreç ölçümü yalnız Melissa CLI sürecinin tepe çalışma kümesini
ve I/O aktarım sayaçlarını kapsar. Diskin fiziksel okuma ve yazma değerleri,
worker veya uzak model RAM’i bu sayaçlardan türetilmez. Pilot sonuçları kendi
vaka kapsamıyla yorumlanır. Değişiklik kabulü için tekrarlanabilir sonuçlar,
güvenlik ve kurtarma regresyonları ve insan incelemesi gerekir.
