# Araç ön kontrolü ve sonuç kayıtları

CLI uzman araçları mevcut yetki, kaynak ve worker kontrollerini kullanır.
Her çağrıdan önce `TaskAdapter`, mevcut kaynak özetini ve kaynak görünürlük
politikasını yakalar. Ortam parmak izi incelenmiş ayar, tarif ve yürütücü kimliğini
bağlar; Docker veya başka bir servisin sağlıklı olduğu iddiası değildir.

Gateway, dispatch öncesinde `tool.preflight`, çağrı sonuçlandığında `tool.receipt`
türünde şifreli kanıt yazar. Yetki/şema nedeniyle çalışmayan çağrılar için de sonuç
kaydı bulunur. Ön kontrol kaydı ile tamamlanmış çağrı kaydı ayrı türlerdir.

Kayıtlar görev, uzman, run, istek, invocation ve attempt kimliklerini; araç adı,
kanonik araç kimliği, şema/argüman özetleri, bilinen adapter sürümü, ortam/kaynak
bağlamı, ön kontrol/çağrı süreleri ve sonuç özetini içerir. Yeni ortak kayıtlarda ham argüman veya ham
araç çıktısı bulunmaz. Tam native kanıtlar mevcut yetkili artifact yollarında kalır.
Raporlanmayan provenance alanları `null` kalır. Sonuç araç çağrısına aittir;
`task_accepted` daima `false` olur. Sürecin çıkış kodu gerekiyorsa native kanıttan
okunur; genel dispatcher adına sıfır çıkış kodu uydurulmaz.

| Durum | Anlamı |
| --- | --- |
| PASS | Dispatcher başarılı ve sonucu belirsiz olmayan gözlem döndürdü. |
| FAIL | Dispatcher bilinen başarısız gözlem döndürdü. |
| BLOCKED | Yetki/politika işlemi reddetti. |
| NOT_RUN | Şema, kaynak ön kontrolü veya iptal nedeniyle araç başlamadı. |
| INCONCLUSIVE | Başlayan çağrının sonucu belirsiz veya iptal durumu var. |

Modelin yazdığı başarılı test iddiası bu kaydı üretmez. PASS, görevin kabulü
değildir. Kaynak özeti, dispatch öncesi atomik olmayan host snapshot'ıdır;
native patch/test kanıtları kendi kaynak ve yan etki kontrollerini korur.

## Hassas kaynaklar

Normal `Repository::open` kaynağında `.env` ve `.env.*` özel dosyaları, bilinen
kimlik bilgisi dizinleri ve anahtar dosyası uzantıları varsayılan olarak dışlanır.
`.env.example`, `.env.sample`, `.env.template`, `.env.dist` okunabilir şablonlardır.
Kurallar alt dizinlerde ve Windows büyük/küçük harf eşleşmesinde uygulanır.

Doğrudan okuma, sayfalı kaynak okuma, arama/keşif, indeks ve worker girdisi aynı
görünürlük denetimini kullanır. Yeniden açılan repository aynı politikayı korur.
Görünürlük sürümü ve tam host politikası kaynak kimliğine bağlanır.
`.gitignore` yalnız keşif tercihidir; secret erişim izni değildir.

Güvenilen host `SourceVisibilityPolicy::source` ve
`Repository::open_with_source_policy` ile tam dosya yoluna istisna tanımlayabilir.
İstisna `.git` erişimini veya açık exclude kuralını geçersizleştirmez. CLI/model
argümanı bu istisnayı kendiliğinden veremez.

Git'in tam worktree capture'ında hassas yol varsa Git worker başlatılmaz.
Bu, eksik dosya kopyasından yanlış deleted/diff sonucu üretmeyi önler. Tam Git
iş akışında hassas dosya bulunan depolar için daha dar history/capture desteği
gereklidir. Paket capture'ı da hassas yol varsa açıkça reddedilir.

Bu dosya adı kuralları her tür secret'ı içeriğinden tespit eden bir tarayıcı değildir.
Mevcut provider credential çözümleme, kaynak dışında tutma, redaction ve secret
tarama kontrolleri korunur. Yeni genel servis secret aktarımı kendi çalışma
yetkisiyle bağlanmalıdır. Eski kanıtlar otomatik silinmez veya yeniden yazılmaz.

## Sağlayıcı uyumu ve güvenli tekrar

`config show` model listesinde `compatibility.basis` alanı
`configured_not_live_verified` değeridir: tool/JSON/vision/stream ayarları canlı
test sonucu diye gösterilmez. Responses ve Messages protokol seçimi bütün
Chat Completions uçlarıyla uyum anlamına gelmez.

CLI, transient tekrar için yalnız transport'un `NotSent` olarak kanıtladığı,
bilinen sıfır kullanımlı girişimleri kullanır. Yeni girişim ayrı istek kimliğiyle
kaydedilir; fallback kendiliğinden açılmaz. Olası gönderim, yarım stream veya
kullanımı belirsiz HTTP hatası yeni çağrı için otomatik tekrar yetkisi vermez.
Bu durumlar mevcut muhafazakâr maliyet rezervasyonunu korur.

Sağlayıcının Retry-After süresi erken retry için kırpılmaz. Seçili bekleme
politikasına sığmayan minimum süre varsa otomatik tekrar yapılmaz. Sağlayıcı
genel olarak tekrar önerse bile ücret/yan etki belirsizliği ayrıca çözülmelidir.

İlgili resmi hata açıklaması:
https://developers.openai.com/api/docs/guides/error-codes
