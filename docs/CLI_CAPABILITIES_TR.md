# CLI geliştirme bağlantıları — 0.3.0

Bu sürüm, mevcut Rust çekirdeğinin Code araçlarını CLI yürütmesine bağlar.
Aktif kapsam 12 Code ve 6 Core profilidir. Diğer alanlar yeniden açılmadı.
Canlı AI kabulü bu çalışma kapsamında **NOT_RUN**; sağlayıcı çağrısı ve ücretli
çıkarım yapılmadı. Yerel testler model kalitesi veya temiz Windows kabulü değildir.

## Bağlanan yetenekler

| Alan | Davranış | Sınır |
| --- | --- | --- |
| Araç ön kontrolü ve sonuç kaydı | Host kaynak/ortam bağlamı, şema/argüman özetleri, ayrı `tool.preflight` ve `tool.receipt` kanıtları | Araç PASS sonucu görev kabulü değildir; çalışmayan veya belirsiz işlem başarı sayılmaz. [Ayrıntılar](TOOL_ADAPTERS_TR.md). |
| Konuşmalı oturum ve proje talimatı | Plan/review/develop; kapsam farkları, kalıcı mesajlar, hedefli soru/yanıt ve çalışan görev kontrolü; kullanıcı/kök/alt klasör AGENTS.md yenilemesi | Hazırlık tek başına yürütme izni değildir. Açık proje sözleşmesi, ortam, bütçe ve onayla genel yürütme etkinleşir. Canlı model token akışı yerine aşama/olay ilerlemesi gösterilir. [Ayrıntılar](CONVERSATION_TR.md). |
| Genel proje ortamı ve süreçleri | Manifestten komut keşfi; projeye özel image/sürüm seçimi; Cargo, npm/pnpm/yarn, uv/pip ve mise; uzun süreç, ham stdout/stderr, stdin, poll/wait/cancel | Host mount/ağ/global kurulum yok; eksik ortam BLOCKED. Salt okunur uzman yalnız onaylı komutları çalıştırır. Son kaynakta komut kabulü ayrı yürütülür. [Ayrıntılar](PROJECT_RUNTIME_TR.md). |
| Büyük depo ve refactor | Erken ignore, kaynak gövdesi reuse, gerçek LSP tip/alias/monorepo gezintisi, etki adayları ve ast-grep önerileri | Semantik rename/build grafı garantisi değildir; uygulama mevcut scoped patch yolundadır. [Ayrıntılar](REPOSITORY_SCALE_TR.md). |
| Proje belleği | `memory_search` ve `memory_propose`; proje kimliğine bağlı şifreli kayıt, yeni oturumdan açma, kaynak yeniliği kontrolü | Öneriler operatör kabul edene kadar geri çağrılmaz. Başka proje bilgisi karışmaz; değişen kaynak eski bilgiyi geçersiz kılar. |
| Kullanıcı/proje eklentileri | Yeniden derlemeden SKILL.md import/katalog/tam yükleme/resource/helper; onaylı HTTP/stdio MCP; kurulu sürümden Context7/resmi doküman; native hook'lar | Paketler görev onayına sabitlenir. Dış metin yeni izin vermez; belirsiz çağrı tekrarlanmaz. Gerçek servis ve canlı model kabulü ayrıca gerekir. [Ayrıntılar](EXTENSIONS_TR.md). |
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
Eski sabit SDK araçlarında tek işlem için 90 saniye duvar süresi vardır; genel
proje süreçlerinde süre onaylanan proje ortamı ve görev bitiş zamanından gelir.
Genel proje modunda sabit SDK build/test araçları yerine seçilen ortam kullanılır.
Yalnız HTML/CSS/SQL gibi bu grammar/SDK kümesi dışındaki kaynaklarda erken
kontrol `NOT_RUN` kalır; Python kontrolü uydurulmaz. İlgili domain araçları ve
ayrı son doğrulama kullanılmaya devam eder. Karma depolarda kontrol dışındaki
dosyalar `unsupported_files` listesinde açıkça gösterilir.

Hindsight kurulmadı veya bağlanmadı. Buradaki bellek mevcut yerel çekirdeği kullanır.

### Kaynak erişimi ve kayıt bütünlüğü

`SourceVisibilityPolicy`, genel çalışma klasörü okuma yetkisinin içindeki
kaynak kapsamını belirler. Dışlanan dosyalar doğrudan okuma, hash, sayfalama,
arama, sembol, anlamsal konum, bağlam ve bellek yollarından geri alınamaz.
Windows dosya kimliği, alias ve reparse denetimleri ayrıca uygulanır.
Kaynak politikası dışlanan yolların adını gösterebilir; dosya içeriğini veya
içerik özetini göstermez. Git/paket için ayrı host yakalama politikaları korunur.

Depo araç çıktıları kök kimliği ve kaynak politikası özetiyle bağlanır.
Farklı kapsamda üretilmiş veya bu bağı taşımayan eski sonuçların sayfalanması
reddedilir; güncel yetkiyle yeniden gözlem gerekir. Eski kanıtlar silinmez.

Bellek önerileri en fazla 128 benzersiz kaynağa ve toplam 8 MiB kaynak
içeriğine dayanabilir. Saklanan veri kaynakların kopyası değil referansıdır.
Bir kayıt en fazla 2 MiB, kitap 4096 kayıt ve kodlanmış toplam 16 MiB olabilir;
inceleme kanıtı referansı en fazla 4096 byte'tır. JSON kaçışları da sayılır.
Öneri ve inceleme bu sınırları değişiklikten önce denetler; başarısız inceleme
önceki kabul edilmiş bilgiyi geçersiz kılamaz. Eski gömülü kaynaklı kayıtlar
aynı sınırlar içinde doğrulanıp referansa çevrilerek okunur.

Checkpoint yazma ve okuma aynı 16 MiB JSON zarfını kullanır; büyük konuşma
geçmişi ayrı mesaj nesnelerinde kalır. Sığmayan veya okunamayacak JSON yeni
checkpoint yayımlanmadan reddedilir, önceki geçerli kayıt korunur. Bunlar host
kayıt sınırlarıdır; modelin bağlamı, düşünme veya çıktı-token ayarı azaltılmaz.
Tamamlama kaydı, doğrulanan görev sürümü ve workflow sırasına bağlıdır;
iptal, süresi dolmuş lease veya eşzamanlı uzlaştırma eski sonucu kabul ettiremez.

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

## Uyarlanan sıralı uzman yürütmesi

`task prepare`, dosya kapsamını, depo büyüklüğünü, kaynakta görülen import
ilişkilerini, test kapsamını ve işlem riskini birlikte değerlendirir.
Karmaşıklık sonucu `simple`, `moderate`, `complex`, `long_horizon` veya
`high_risk` olur. Import ilişkileri adaydır; anlamsal çözümleme kanıtı
olarak gösterilmez. İnceleme sınırına ulaşılırsa `incomplete` açıkça yazılır.
Maliyet tahmini için ölçüm yoksa değer bilinmiyor kalır; yetkili süre ve
maliyet yalnız operatörün kabul tarifinden alınır.

Basit bir uygulama değişikliği implementer → test-engineer → reviewer
sırasını kullanır. Mimari işlerde repository-analyst ve architect,
derleme sorunlarında debugger, yüksek riskte security-reviewer eklenir.
Yalnız mevcut katalogdaki çalıştırılabilir profiller seçilir.
Salt okunur görevlerde yazma yetkisi verilmez.

Yeni yürütülebilir CLI görevlerinde bağımsız test-engineer ve son reviewer
eksikse görev sözleşmesi oluşturulmadan eklenir; onay ekranı bütün seçilen
profilleri gösterir. En az üç seri adım `adaptive_serial` yürütmesini
kullanır. Aynı anda bir uzman çalışır. Son yazma işleminden sonra test ve
bağımsız inceleme zorunludur. Önceden kaydedilmiş görevlerin sözleşmeleri
ve genel çekirdek API'leri değiştirilmez.

Uzman `task_plan_propose` ile kalan adımlar için öneri kaydedebilir.
Öneri güncel kaynak ve plan özetiyle bağlanır, yalnız ilk onaylanan uzman
şablonlarını kullanır ve mevcut uzman bitince uygulanır. Model yeni araç,
dosya kapsamı, yetki, kabul ölçütü veya bütçe belirleyemez. Tamamlanmış
adımlar korunur. Aynı planın tekrarı ve sınırsız yeniden planlama reddedilir.

Kalıcı görev kaydı hedefi, tamamlanan adımları, değişen dosyaları, aday
bulguları, başarısız denemeleri, gerçek kontrol kayıtlarını ve kalan bütçeyi
saklar. Uzmanlar kısa aday özetleri ve kanıt referanslarıyla devralır.
Bütün konuşma geçmişi devir paketine kopyalanmaz. `task_evidence_read` ile
kayıtta referansı bulunan aday veya genel kontrol kanıtları sayfalanabilir;
özel kabul çıktıları ve rastgele kayıt kimlikleri bu araçtan okunamaz. Aday bulguları doğrulanmış
olgulardan ayrı tutulur; kaynak değişince eski başarılı kontrol bilgisi
güncel doğrulama sayılmaz. Modelin aday üretmesi görevi kabul etmez;
kabul edilen çıktı yalnız korumalı host doğrulamasından sonra kaydedilir.

`task progress`, bu kayıtla birlikte yürütücünün kararını gösterir.
Tekrarlanan kontrol başarısızlığı, bütçe/süre dolması ve sonucu belirsiz
işlemler ilerlemeyi durdurur. Plan değişimi önceki maliyeti, başarısızlıkları
ve bitiş zamanını sıfırlamaz. Uzman ataması ve devir paketi aynı kalıcı
checkpoint içindedir; bu sınırdaki kesinti yeni uzman veya mükerrer çağrı
oluşturmadan toparlanabilir.

Bu bağlantılar yerel protokol ve süreç testleriyle sınanır. Canlı modelin
plan kalitesi ve görev başarısı ayrıca ölçülmelidir.

## Model havuzu ve araç profilleri

Mevcut ayar dosyasındaki model havuzun ilk üyesidir. İsteğe bağlı
`model_pool` listesine `{"id":"ikinci","settings":{...}}` biçiminde üyeler
eklenebilir. Her `settings` nesnesi, iç içe havuz alanı olmadan, ana
[ayar örneğindeki](../examples/cli/settings.template.json) tam model
alanlarını kullanır. Model kimliği, protokol, fiyat, bağlam kapasitesi ve
anahtar referansı her üye için ayrı tanımlanır. `config show` ve görev
onayından önceki plan, havuzu anahtar referanslarını göstermeden listeler.
Eski tek model ayarları çalışmaya devam eder.

Her üyede `tool_profile` olarak `precision`, `terminal` veya `hybrid`
seçilebilir. Varsayılan `precision`, kaynak gezintisi, kesin patch ve
tipli SDK işlemleri kullanır. `terminal`, görevde izinli SDK işlemlerini
`sandbox_check` ve `sandbox_tests` gibi kısa komutlarla sunar. Komutlar
çalışma dizini, ortam değişkeni veya serbest argv kabul etmez.
`hybrid`, yapılandırılmış gezinti ve testleri seçili kısa komutlarla
birleştirir. Araç şemaları uzman, görev ve o anda erişilebilir araçlara
göre süzülür. Saklı veya kapsam dışı çağrılar yürütülmez.

`measurements` nesnesinde isteğe bağlı `task_success_ppm` ve
`p95_latency_ms` verilebilir. Ölçüm yoksa değer bilinmiyor kalır.
Basit işlerde maliyet ve gecikme, karmaşık iş ve incelemelerde ölçülmüş
kalite öne çıkar. Havuz seçiminde görünen ilk bağlamın UTF-8 boyutu ve
tam bağlam üzerinden hesaplanan maliyet üst sınırı kullanılır. Gateway,
gönderilen isteği ve kalan bütçeyi ayrıca doğrular.

Seçilen model, onaylı ayarlar ve uzman oturumuyla kaydedilir. Aynı oturum
devam ederken seçim değişmez. Yeni uzman kendi model oturumunu açar;
önceki sağlayıcının yerel protokol durumu aktarılmaz. Sağlayıcı çağrılarının
maliyeti aynı görev bütçesinden düşer. Sonucu belirsiz işlemler yeni
yönlendirme için durdurma sebebidir.

## Uzun görevler ve teşhise dayalı onarım

Görevin kalıcı ölçüm kaydını `melissa --json task metrics TASK_ID` ile
okuyabilirsin. Çıktı doğrulanmış kabulü, model gönderim girişimlerini, araç
denemelerini, süreyi, bağlamı ve kayda geçen token kullanımını gösterir.
`known_total` yalnız bildirilen örnekleri toplar. `missing_samples` eksik
örnekleri ayrı sayar. Sağlayıcı kullanım bildirmezse ilgili toplam boş kalır.
Maliyet, yerel fiyat kartı ve rezervasyon kayıtlarından gelir. Görev süresi
ilk çalışma kaydından bitişe kadardır ve aradaki duraklamaları içerir.
Model kanıtı yazma süresi, yeni alındılarda istek ve yanıt kanıtlarının
yazımından ölçülür. Süreç RAM’i ve I/O sayaçları deney koşucusunun ayrı
ölçümleridir. [Harness Lab rehberi](HARNESS_LAB_TR.md)

Görev onayı, toplam duvar süresi ve bitiş zamanını belirler. Model
isteğinin zaman aşımı model ayarından, SDK işleminin sınırı worker
politikasından gelir. Toplam süre dolduğunda yürütücü çalışmayı durdurur.
Süre veya harcama plan değişiminde yeniden başlatılmaz.

Uzman devrindeki teşhisler hata sınıfı, güncel kaynak özeti, dosya/satır,
varsa sembol, başarısız test kimliği, son değişiklik ve kanıt referansını
taşır. Aynı kaynakta tekrarlar sınırlanır. Kaynağı ve işlemi aynı olan
başarılı sabit kontroller daha önceki gerçek kanıtı kullanabilir. Çıktı
bunun bir tekrar çalıştırma olmadığını açıkça belirtir.

Güvenli model turu, kaynak görüntüsünü ve bütçeyi aynı kalıcı kayıtla
bağlar. Uzman bitişindeki görev kaydı, plan, kaynak ve görev sürümünü de
saklar. `task resume` eski yürütücünün durduğunu, kaynağın güncelliğini,
görev sürümünü ve muhasebeyi kontrol eder. Sonucu belirsiz bir çağrı veya
çalışan worker otomatik olarak yeniden başlatılmaz.

Açıkça tanımlanmış geliştirme testleri `code_tests` ile çalışır.
İkiden fazla açık test varsa `code_targeted_tests`, operatörün listesinde
bulunan en az iki farklı vakayı seçebilir. Özel kabul testleri yalnız
son bağımsız doğrulayıcıya açıktır. Mimari ve geniş değişikliklerde
yapılandırılmış açık test paketinin tamamı güncel kaynakta geçmelidir.
Güvenlik uzmanı seçilen görevlerde son doğrulamaya izole statik tarama
da eklenir. Tam yayın, lisans ve paket güvenliği ayrıca kabul ister.

Güncel, onarılabilir bir hata kalırsa seri yürütücü ilk onaylı yazıcı,
test uzmanı ve inceleme şablonlarından sınırlı bir onarım planı kurar.
Onarım güncel kaynakta geçen test kanıtı gerektirir. Eksik SDK, belirsiz
etki veya aynı teşhisin sürmesi kendi koşullarıyla durdurulur.

## Görev hazırlamak

```powershell
melissa --json task prepare --workspace C:/Projects/example --goal 'src/main.py içindeki hatayı düzelt' --out C:/MelissaPrivate/draft.json
```

Çıktı kaynak özeti, önerilen uzmanlar, olası yazma kapsamı ve eksik kabul
girdilerini içerir. Varsayılan taslak `ready_to_create:false` (özel yürütme
tarifi hazır değil), `ready_to_record_project_task:true` olur. Terminalde
`/create` özel oracle olmadan bu hazırlığı kaydeder; model yapılandırması,
açık `/budget` ve kabul komutları varsa onaya bağlı genel proje yürütmesini
oluşturur. [Ortam bağlantısı ve süreçler](PROJECT_RUNTIME_TR.md). Kabul tarifi
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

Her model isteği için güncel hedef/adım ve araç geçmişi yeniden hazırlanır.
Uzun gözlemler tam kayıt bağlantılarıyla kısaltılabilir; çıktı/düşünme ayarları
değişmez. Ayrıntılar ve destek sınırları: [bağlam yönetimi](CONTEXT_TR.md).

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

Proje kontrolleri, erken doğrulama, gerçek proje debug ayarı ve hata tekrar üretimi: [doğrulama rehberi](VERIFICATION_TR.md).

Göreve ait servisler, API sözleşmesi ve ayrı DB migration provası: [servis rehberi](SERVICES_TR.md).
