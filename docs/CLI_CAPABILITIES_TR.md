# CLI geliştirme bağlantıları — 0.3.0

Bu sürüm, mevcut Rust çekirdeğinin Code araçlarını CLI yürütmesine bağlar.
Aktif kapsam 12 Code ve 6 Core profilidir. Diğer alanlar yeniden açılmadı.
Canlı AI kabulü bu çalışma kapsamında **NOT_RUN**; sağlayıcı çağrısı ve ücretli
çıkarım yapılmadı. Yerel testler model kalitesi veya temiz Windows kabulü değildir.

## Bağlanan yetenekler

| Alan | Davranış | Sınır |
| --- | --- | --- |
| Proje belleği | `memory_search` ve `memory_propose`; proje kimliğine bağlı şifreli kayıt, yeni oturumdan açma, kaynak yeniliği kontrolü | Öneriler operatör kabul edene kadar geri çağrılmaz. Başka proje bilgisi karışmaz; değişen kaynak eski bilgiyi geçersiz kılar. |
| Skill ve bağlam | Pinin tamamını yükleme; kaynak arama, template/factory, gerçek izole Python helper, ortak scriptler | Model komut, interpreter, kök klasör veya yetki seçemez. Helper sonucu aday/gözlemdir. |
| Anlamsal gezinti | `code_semantic`: gerçek rust-analyzer, TypeScript dil sunucusu veya Pyright üzerinden definition, references, symbols, hover, diagnostics | Dil sorgulanan dosyadan seçilir. Konumlar sıfır tabanlı UTF-16; build scriptleri kapalı. Eski `code_references` yalnız sözdizimsel aramadır. |
| Erken kontrol | Başarılı patch/create/delete/move sonrasında güncel kaynakta grammar ve yetkili SDK check | `mutation_applied:true`, yazının zaten uygulandığını belirtir. Kontrol başarısızsa eski patch tekrar gönderilmez. Bu kontrol son kabul değildir. |
| Görev hazırlama | Doğal dil amacı + kaynak envanteri ile incelenebilir uzman sırası ve dosya kapsamı taslağı | Yerel niyet kuralları kullanılır; LLM planı değildir. Özel kabul sonuçları, negatif kontrol, maliyet/süre yetkisi uydurulmaz. |
| Depo çalışması | Sabit SDK build/check/lint/format/test/doc/e2e, test keşfi, kaynak breakpointli DAP; Git status/diff/log ve özel worktree | Desteklenen Rust/Cargo, JS/TS, Python sözleşmeleri geçerlidir. Rastgele shell, host attach, commit/push veya üretim yazması açılmaz. |
| Uzun iş/yeniden plan | Şifreli tam checkpoint/geçmiş korunur; sonuç sayfaları yeni dispatcher üzerinden açılır; `task replan` kalan uzman adımlarını değiştirir | Plan başına 128 adım. Eski 16 adım/30 dakika tarife sınırı kaldırıldı; süre operatör tarifinden gelir. İlk yetki, kabul, ortak bütçe ve bitiş zamanı değişmez. |

Model bağlamı ve çıktı-token ayarları düşürülmedi. Çekirdeğin mevcut sonlu
istek/araç, disk, worker ve geçmiş boyutu sınırları geçerlidir; sınırsız çalışma
iddiası yoktur. Geçmiş otomatik özetlenip silinmez. SDK işlemleri mevcut sabit
image ve ağsız izole worker kullanır; bağımlılık/SDK yokluğu PASS sayılmaz.
CLI'nın bu geliştirme worker bağında tek işlem için 90 saniye duvar süresi vardır.
Yalnız HTML/CSS/SQL gibi bu grammar/SDK kümesi dışındaki kaynaklarda erken
kontrol `NOT_RUN` kalır; Python kontrolü uydurulmaz. İlgili domain araçları ve
ayrı son doğrulama kullanılmaya devam eder. Karma depolarda kontrol dışındaki
dosyalar `unsupported_files` listesinde açıkça gösterilir.

Hindsight kurulmadı veya bağlanmadı. Buradaki bellek mevcut yerel çekirdeği kullanır.

### Uzun oturum ve kayıt davranışı

Görev süresiyle tek API isteğinin bekleme süresi ayrıdır. `request_timeout_ms`
varsayılanı 30 dakikadır; bu değer bütün görevi 30 dakikada kesmez.

Her gönderimden önce gerçek protokol gövdesi hazırlanıp denetlenir. Sistem,
görev, skill metinleri, araç/çıktı şemaları ve sağlayıcının opaque continuation
alanları değiştirilmez. Bağlam dolduğunda eski araç gözlemleri, tam içerikleri
şifreli kayıtta tutularak çalışma görünümünde referansla temsil edilebilir.
`history_read` aynı görev/oturumdaki kaydın istenen aralığını geri getirir.
Son gözlemler ve araç çağrısı/sonucu eşleşmeleri korunur; gerekli içerik hâlâ
sığmıyorsa istek gönderilmeden açıklayıcı hata döner.

Varsayılan sayım UTF-8 gövdesi ve protokol payı üzerinden muhafazakâr bir
kontroldür; gerçek model tokenizer ölçümü veya fatura değildir. Çekirdek tam
endpoint/model için kayıtlı tokenizer adaptörünü destekler. Bu adaptör yoksa
maliyet rezervasyonu model kapasitesini esas almaya devam eder. Görseller için
eşleşen sayıcı yoksa rapor `not_measured_multimodal` yazar; görseli küçültmez
ve doğrulanmış token kapasitesi iddiasında bulunmaz.

İsteğe bağlı `local_tokenizer` ayarıyla yerel bir Hugging Face `tokenizer.json`
dosyası bağlanabilir: `{"path":"C:/MelissaPrivate/tokenizer.json","blake3":"DOSYANIN_64_KARAKTER_KUCUK_HARF_BLAKE3_OZETI"}`.
Dosyayı güvenilir model yayımından ayrıca temin edin; uygulama dosya indirmez,
uzak Python kodu çalıştırmaz ve model anahtarı okumaz. Import ve çalıştırmada
dosya özeti doğrulanır. Endpoint, protokol, model veya özet değişirse yürütme
politikasının kimliği de değişir. Alan eklenmezse eski ayar özeti korunur.

`wire_token_measurement`, tüm native JSON metninin gerçek tokenizer ile
sayımıdır; sağlayıcının chat template'iyle oluşturduğu **model girdisi token
sayısı değildir**. Bu nedenle kapasiteyi veya maliyet rezervasyonunu azaltmaz;
çıktı token ayarını da değiştirmez. Dosya içindeki truncation ve padding sayım
için kapatılır. Görsel/opaque continuation bulunan isteklerde ölçüm yapılmaz.
32 MiB üzerindeki tokenizer dosyaları reddedilir; 2 MiB üzerindeki JSON için
yalnız bu ek ölçüm atlanır, istek kırpılmaz. Bu sınırlar model bağlam limiti değildir.

Yerel dosyanın özetini `cargo run -p melissa-model-gateway --example measure_wire -- --digest C:/MelissaPrivate/tokenizer.json`
ile alın. Çevrimdışı JSON sayımını denemek için aynı örneğe sırasıyla tokenizer
yolu, özet ve native request JSON yolu verin. Örnekte HTTP bağlantısı kurulmaz.
`config show` seçili tokenizer özetini ve `native_wire_only` ölçüm türünü gösterir.

Tam geçmiş tek bir büyüyen JSON dosyası yerine değişmez mesaj nesneleri ve
manifestlerle saklanır. Önceki tam checkpoint biçimi okunmaya devam eder.
Çalıştırma politikası değişmiş eski aktif görevlerin devam girişimi ise
reddedilir; kayıt okunabilmesi, eski yürütme yetkisinin yeni politikaya
otomatik taşındığı anlamına gelmez.
Proje belleği de kaynak dosyalarını kopyalamak yerine kaynak kimliği/özeti
tutar; değişen kayıtlar ayrı nesneler halinde yazılır. Kaynak yeniliği her
yeniden açılışta doğrulanır. Bellek kullanan ajan oturumu açıkken çakışan
yerel bellek incelemesi beklemelidir; aynı anda iki yazıcının değişiklikleri
birbirinin üzerine yazılmaz.

Arama sonucu tam dosya değildir: kaynak özeti, özgün byte aralığı ve sınırlı
alıntı içerir. Toplam kodlanmış sonuç 256 KiB ile sınırlıdır; devamı açık
dosya/aralık okumasıyla alınır. Ayrıştırma ve FTS indeksi oturum içinde
yalnız değişen dosyalar için güncellenir. İçerik özeti doğrulaması devam eder;
bu özellik bütün dosya okumalarının kaldırıldığı anlamına gelmez.

Windows CLI deposunda doğrulanmış kayıtlar yazma/silmeyi engelleyen dosya
tutamaçlarıyla yeniden kullanılır. Yeni dosyalar doğrulanır; GC önce bu geçici
tutamaçları bırakır. Diğer platformlarda tam doğrulama yolu kullanılır.
Worker aktarımı en çok 1024 dosya/64 MiB kabul eder ve kopyalama öncesinde
denetlenir. Ustar aktarımı 155 byte önek/100 byte son ad alanına sığan uzun
yolları destekler; desteklenmeyen yol veya boyut açık hata olarak kalır.

## Görev hazırlamak

```powershell
melissa --json task prepare --workspace C:/Projects/example --goal 'src/main.py içindeki hatayı düzelt' --out C:/MelissaPrivate/draft.json
```

Çıktı kaynak özeti, önerilen uzmanlar, olası yazma kapsamı ve eksik kabul
girdilerini içerir. Varsayılan taslak `ready_to_create:false` olur. Kabul tarifi
hazırsa `--acceptance C:/MelissaPrivate/acceptance.json` eklenebilir: özel oracle,
süre/maliyet ve dosya yetkileri şablondan alınır; öneri şablonun dosya kapsamını
genişletemez. Ortaya çıkan tarif yine incelenir ve normal `task new`, kaynak
bağlı `task approve`, `task run` akışından geçer. `prepare` model veya worker başlatmaz.
Taslak/özel tarif proje dışında yazılır; mevcut çıktı dosyası ezilmez.

## Belleği incelemek

```powershell
melissa --json memory list --workspace C:/Projects/example
melissa --json memory accept --workspace C:/Projects/example --id MEMORY_ID --digest REVIEW_DIGEST
melissa --json memory reject --workspace C:/Projects/example --id MEMORY_ID --digest REVIEW_DIGEST
melissa --json memory forget --workspace C:/Projects/example --id MEMORY_ID --digest REVIEW_DIGEST
```

`REVIEW_DIGEST` güncel listenin tamamını bağlar; eski listeyle kabul verilmez.
Aynı anahtardaki bir önerinin kabulü önceki kabul edilmiş kaydı geçersiz kılar.
Bir görevin kapsam dışında tuttuğu kaynak o görevde bellekten görünmez; bu
durum tek başına proje genelindeki kaydı silmez. `forget` geri çağırmayı kapatır
ve güncel kaydı temizler; eski şifreli denetim kopyalarını fiziksel olarak silmez.
Bellek talimat veya yeni yetki kaynağı değildir.

## Güvenli yeniden planlama

```powershell
melissa task pause TASK_ID
melissa --json task progress TASK_ID
melissa --json task replan TASK_ID --recipe C:/MelissaPrivate/revised.json
melissa task resume TASK_ID
```

Önce `paused` kaydı görülmelidir. Yeni tarif yalnız `agents` listesini
değiştirebilir ve ilk görevde yetkilendirilen profilleri kullanabilir. Çalışan
ve tamamlanan adımlar aynı kalır; korunan kabul, yazılabilir dosyalar, negatif
kontrol, süre ve bütçe değişmez. Çalışan adım varsa revizyon sıraya alınır ve
o adım tamamlanınca uygulanır. Kaynak pause sonrasında değişmişse veya belirsiz
işlem varsa revizyon reddedilir. `resume` normal model yetkisiyle çalışabilir;
bu geliştirmede çağrılmadı.

## Araç kullanımı

Model araçları yalnız ilgili uzman/görev kapsamıyla sunulur. `context_script`
tanımı kullanılabilir scriptlerin gerçek girdi şemalarını içerir. Native
`inspect_environment`, `build_symbol_index`, `generate_patch_preview` ve
`run_targeted_tests` kaynak/SDK araçlarına bağlıdır; sonuncusu keşfedilen mevcut
unit/integration paketini çalıştırır, model seçimiyle keyfi komut çalıştırmaz.
Kayıtlı build/lint/test işlemleri sabittir; özel test harness kaydı CLI tarifinden
otomatik oluşturulmaz. Desteklenmeyen proje/harness açık hata olarak kalır.

Git, depo ve metadata için bounded tam kopya gerektirir. Açık `--exclude`
kapsamı varsa Git kopyası açılmaz; limit aşımında kısmi status üretilmez.
Credential helper, hook ve dış diff kullanılmaz. Oluşturulan worktree yalnız
worker içindedir; host checkout değildir. Çıktı tar arşivi kanıt olarak tutulur,
hosta otomatik çıkarılmaz. Büyük/linked worktree veya dış object store desteği
çekirdeğin mevcut sınırlarına tabidir.

## Yerel doğrulama

`crates/cli/src/capabilities_tests.rs` yeni bağlantıların davranış ve sınırlarını
test eder. Normal testler bellek, 12 rol, script bağlantıları, taslak ve yeniden
planlamayı kapsar. Dört ayrı Docker testi gerçek dil sunucusu, test keşfi/helper,
geçersiz regex kontrolü, Git ve DAP yolunu çalıştırır. Bu dört test varsayılan
test setinde `ignored` olup ayrıca açıkça çalıştırılır; canlı AI testi değildir.

```powershell
cargo test -p melissa-cli --offline
cargo test -p melissa-cli --lib capabilities_tests::native_cli --offline -- --ignored --test-threads=1
cargo clippy -p melissa-cli -p melissa-build-test -p melissa-skill-context --all-targets --offline -- -D warnings
```

Test kayıtları özel geliştirme klasöründe tutulur. Eski başarısız denemeler
silinmez; son test sonucu onları geçmişte başarılı olmuş gibi değiştirmez.
Bu sürüm için yeni canlı model, büyük depo karşılaştırması, temiz Windows,
dağıtım ZIP'i veya public yayın kabulü iddia edilmez.
