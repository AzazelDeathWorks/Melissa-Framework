# Canlı önizleme ve frontend doğrulama

Genel proje servis sözleşmesindeki `browser` ayarı, gerçek uygulama servisini
Chromium/Playwright ile aynı görev kapsülünde açar. Frontend, API ve özel DB aynı
yakalanmış kaynak sürümüne bağlanır. Model çağrısı gerektirmeyen operatör yolu ve
ajan araçları aynı kayıt/izin/süre mekanizmasını kullanır.

## Başlatma

1. Projeye uygun SDK'yı [frontend toolchain](../scripts/toolchains/frontend/README.md)
   ile hazırlayın ve image kimliğini proje ortamında seçin.
2. [Servis örneğini](../examples/cli/project-browser.template.json) gerçek projenin
   komutlarına, portlarına ve CSS seçicilerine uyarlayın.
3. `/services JSON_DOSYASI` veya `services configure --workspace PATH --path JSON`
   ile ayarı yükleyin. Yeni görev oluşturup kaynak/komut/ayar kapsamını inceleyin
   ve onaylayın. Ayar değişikliği eski görevin yetkisini değiştirmez.

```powershell
melissa --json services session TASK
# JSON komutları veya /close; EOF da sahip olunan oturumu kapatır.
```

Browser ayarı varsa foreground oturumu `preview_ready` kaydında gerçek yerel HTTP
adresi verir. İlk bağlantı tek kullanıcılı önizleme yeteneğini HttpOnly,
SameSite=Strict cookie'ye taşır. Adres yalnız 127.0.0.1 üzerinde, oturum açıkken
çalışır. Docker portu, host dosyaları veya daemon socket'i paylaşılmaz. Kaynak
değişirse eski önizleme yeni kaynakmış gibi sunulmaz; isteğe 409 döner. İptal ve
oturum kapanışı listener'ı da kapatır. Adresteki erişim belirtecini paylaşmayın.

Bu HTTP köprüsü gövdeleri kaynak bağlı kanal üzerinden taşır; CORS, üçüncü taraf
oturum açma, cookie/domain yeniden yazımı, streaming, WebSocket upgrade ve host
service-worker çalıştırma için genel bir reverse proxy değildir. Uygulamanın
WebSocket/HMR, service worker ve oturum davranışını aşağıdaki görev içi gerçek
browser ile doğrulayın. Yönlendirmeler otomatik takip edilmez.

## Ajan ve operatör araçları

`project_services`: `start`, ardından tam `session_id` ile `browser` operasyonu;
işlem bittiğinde `close`. İç browser komutları:

- `navigate`, `inspect`, `elements`, `fill`, `click`, `press`, `wait`, `viewport`,
  `screenshot`: kayıtlı uygulamada gerçek etkileşim.
- `verify`: yalnız adı onaylanmış frontend senaryosu.
- `accessibility`: bulunduğu sayfada yerel axe-core taraması.
- `capabilities`: bu görevdeki WebSocket, service worker ve DevTools tercihleri.
- `devtools`: yalnız `diagnostics` veya `performance`; aynı görevin Chromium'u.

```json
{"operation":"browser","command":{"command":"verify","name":"main-flow"}}
```

```powershell
./scripts/browser-verify.ps1 -Task TASK -Check main-flow
```

`service_result_page`, aynı görevin servis kayıtlarını ve browser binary/metadata
kanıtlarını byte sayfalarıyla okur. `frontend_image`, tam PNG artifact kimliğiyle
görüntüyü destekleyen modele gerçek PNG olarak iletir. Model vision desteği ve
görevin ilgili araç yetkisi ayrıca gerekir; metin araç yanıtı görsel inceleme
yerine geçmez. Host, görüntü bytes/digest/metadata/servis receipt bağını doğrular.

## Senaryolar ve ayrı sonuçlar

Her senaryo adlandırılmış dar/geniş viewport'larda aynı incelenmiş adımları
uygular. `navigate` ile başlar; seçici üzerinden `fill`, `click`, `press`, `wait`,
`assert_text`, `assert_visible` ve adlandırılmış `checkpoint` kullanır. Çağıranın
JavaScript, keyfi URL, CDP hedefi veya browser executable vermesine izin verilmez.
Her viewport yeni sayfadır; aynı görev içi backend verisi korunur. Gerekli fixture
resetini senaryonun kendisi tanımlamalıdır.

Checkpoint gerçek ekran PNG'sini ve istenmişse axe sonucunu kaydeder. Boş,
yükleniyor, hata ve ana akış durumlarını projenin gerçek davranışına göre açıkça
tanımlayın. Beklemek için zaman tahmini yerine görünür DOM koşulları kullanın.
Sonuçta `functionality`, `accessibility` ve `visual` ayrı kalır. Görsel kontrol
istenmediyse `NOT_RUN`; işlevsel adım daha önce kırıldıysa ulaşılamayan taramalar
çalışmış sayılmaz. Eksik SDK `BLOCKED`, belirsiz araç kesintisi `INCONCLUSIVE`
olur. Başarısız örnekler ve önceki kaynak sürümlerinin kayıtları korunur.

axe yalnız otomatik kuralları sınar. `incomplete` bulguları elle inceleme gerektirir;
0 otomatik ihlal, tam WCAG uyumluluğu veya güzel tasarım garantisi değildir.

## Kullanıcının kabul ettiği görsel referans

İlk koşu baseline olmadan ekran kanıtı üretir. Ekranı dışa aktararak inceleyin:

```powershell
melissa task evidence TASK --artifact PNG_ARTIFACT_ID --out NEW_SCREENSHOT.png
```

Oturumu kapatın. İlgili `browser_artifacts` kaydındaki tam `artifact_id` ve
`review_digest` ile açık operatör kabulü verin:

```powershell
melissa --json services baseline-accept TASK --request-json '{"artifact_id":"PNG_ARTIFACT_ID","name":"approved-main","digest":"REVIEW_DIGEST"}'
```

Kabul edilen kayıt proje klasörünün kimliğine bağlı, şifreli ve değişmez bir
`visual-...` kimliği alır. Kimliği senaryonun `visual` listesine ekleyin:

```json
{"viewport":"narrow","checkpoint":"saved","baseline_id":"visual-<returned-64-hex>","max_diff_ratio":0.0}
```

Yeni ayarla yeni görev oluşturulup onaylanır. Başarısız kıyas baseline'ı yazamaz;
ajanın kabul/güncelleme aracı yoktur. Yeniden kabul her zaman yeni kayıt üretir.
Proje, servis, checkpoint, viewport, Chromium ve Playwright eşleşir; uyumsuz
ortam `BLOCKED`, görüntü boyutu/piksel farkı toleransı aşarsa `FAIL` olur. Güncel
PNG, fark PNG'si, farklı piksel sayısı ve kullanılan referans birlikte korunur.
Pixelmatch piksel eşiği 0.1'dir; izin verilen farklı piksel oranını kullanıcı
incelenmiş ayarda seçer. Bu ölçüm tasarım kalitesi puanı değildir.

## Gelişmiş browser yetenekleri

`websocket`, `service_workers` ve `devtools` varsayılan olarak kapalıdır.
WebSocket yalnız kayıtlı izinli servislere açılır. HMR'nin çalışması uygulamanın
dev server/modül uygulamasına bağlıdır; yalnız bağlantının açılması HMR kabulü
sayılmaz. Form durumunu koruyarak modül değişimini gerçekten sınayın.

Service worker açıkken Playwright HTTP interception kapsamı kısmi olabilir;
özellikle güncellenen worker ana script'i için tam interception iddiası yoktur.
Bu mod aynı görevin kapsülüne güvenir. Dış ağ/host erişimini kapsülün kernel
yalıtımı kapatır; browser route filtresi tek güvenlik sınırı değildir.

DevTools MCP mevcut özel Chromium'a bağlanır. Kişisel browser/profil/CDP adresi
kullanılmaz. Sonlu araç listesi üzerinden konsol/ağ incelemesi ve gerçek yerel
performans trace'i alınır. Yalnız görev içi kayıt klasörü dosya kökü olarak
tanımlıdır; telemetry, CrUX, source map, çağıran JS ve file navigation kapalıdır.
Performans sonucu laboratuvar gözlemidir; gerçek kullanıcı CWV/CrUX verisi değildir.

## Kaynak ve kayıt bağı

Ekran görüntüsü, Playwright trace, tam konsol/ağ olay dosyası ve DevTools trace'i
ayrı artifact'lerdir. Kaynak digest'i, ortam digest'i, servis, viewport ve browser
sürümleriyle bağlıdır. Ham kayıt hassas uygulama verisi içerebilir; model için
güvenilir talimat değildir. Kayıt kapasitesi aşılırsa başarılı veya tam kayıt
iddiası üretilmez.

`acceptance` listesine `{"kind":"browser","name":"main-flow"}` eklemek, son
host kabulünde bu senaryoyu son kaynaktan yeniden başlatılan servislerde zorunlu
kılar. Modelin önceki receipt'i, kendi "bitti" sözü veya eski önizleme yerine
geçmez. Public proje kontrolleri bağımsız özel oracle/yayın kabulü değildir.
