# Tanılama, ölçüm ve güvenli kayıt bakımı

Bu arayüzler operatör içindir; model araç yetkisini genişletmez. Model sağlayıcısı
çağrılmaz. Görev/araç kabulü, tanılama üretimi, Collector aktarımı ve yedek geri
yüklemesi ayrı sonuçlardır. Canlı AI ve temiz Windows kabulü ayrıca gerekir.

## Hatanın kaynağı

### Konuşma arayüzü ve ortak bağlam bütçesi

`Ctrl+P` komut paletini açar; salt okunur görünümler taslağı koruyarak açılır.
Diğer komutlar yalnız iş yokken boş taslağa eklenir ve ayrıca Enter gerektirir.
Taslak varken veya iş sürerken palet yalnız dört salt okunur görünümü sunar;
diğer komutlar için önce taslağı `Ctrl+Q` ile sıraya alabilirsin.
`/` ile komut açıklamaları, Tab ile tamamlama, `/help`
ile bütün kısayollar görünür. Plan, uygulama ve izin incelemesi aynı host
sınırlarından geçer. Salt okunur `/status` API çağırmaz veya iş başlatmaz.
Dar terminalde ayrıntılar `/status` ekranındadır. Değişmeyen ekran yeniden
çizilmez; boyut, imleç/seçim, taslak, araç, arama veya bağlam değişirse çizilir.
Boşta giriş yoklaması 100 ms, aktif işte 20 ms'dir; tuş gelince hemen uyanır.

Sohbet ve Gateway aynı `TokenBudget` hesabını kullanır. Çıktı rezervi önce
kapasiteden ayrılır, asla sessizce düşürülmez. Güvenilir, native istek/model/uç
nokta eşleşmeli sayaç varsa gerçek sayım kullanılabilir. Yoksa native JSON
baytlarına 1024 çerçeve birimi ve mesaj başına 64 birim eklenir; **bu ölçülmüş
token değildir**. CLI üst-sınır göstergesi bu temkinli girdi bütçesinin oranıdır,
sağlayıcının gerçek doluluk yüzdesi değildir. Sağlayıcının son yanıt sayımı ve
yerel JSON tokenizer gözlemi ayrı kalır; sonraki isteğe kesin tahmin sayılmaz.
Görseller/opaque continuation için desteklenmeyen native sayım bilinmiyor kalır.
Bu host politikası sunucunun gizli chat şablonunu/ek bağlamını doğrulamaz;
model kapasitesi ve protokol ayarları operatörün doğru yapılandırmasına bağlıdır.

Sohbetin mevcut otomatik özetlemesi girdi bütçesinin yüzde 80 eşiğini kullanır;
özet de kendi tam bütçe denetiminden geçer. Sığmayan istek kesilip gönderilmez;
özgün kayıt korunur, daha küçük ek veya `/compact` istenir. Gateway mevcut
güvenli araç çıktısı arşivlemesini aynı bütçeye karşı dener. Opsiyonel tokenizer
ölçüm hatası konuşmayı durdurmaz; host kapasite/harcama korumaları sürer.

### Gerçek komut ve container worker profili

```sh
# Açıkça seçilen bir komutu ölçer; komutun kendi izin/ücret politikasını değiştirmez.
python3 tools/profile_task.py --timeout 60 --container EXISTING_CONTAINER -- YOUR_COMMAND
# Ücretsiz, ağsız, root olmayan CPU/RAM fixture; yalnız zaten yerel olan Linux imajı.
python3 tools/worker_profile_smoke.py --image LOCAL_IMAGE --repetitions 3 > worker-profile.json
```

İlk araç Linux/macOS `wait4` CPU'sunu (beklenmiş alt süreçler dahil), süreç ömrü
RSS tepesini ve Linux'ta yalnız kök sürecin örneklenmiş RSS alt sınırını ayırır.
Duvar süresi başlatma ve örnekleme çözünürlüğünü içerir. Komut argümanları,
ortam değerleri ve çıktı içerikleri rapora alınmaz. Başarısız/timeout komutları
exit code ve duruş nedeni ile korunur. Timeout yalnız host süreç grubunu öldürür;
container içindeki işi durdurduğunu kanıtlamaz. Mevcut container durdurulmaz;
worker iptali sahibi olan harness'in sorumluluğudur.

Linux'ta seçilen container'ın hostta görülebilen cgroup-v2 CPU ve block-I/O sayaç
farkları ölçülür. RAM örnek tepesinin kaçırılmış zirveleri olabilir; kernel
`memory.peak` ise tüm cgroup ömrüdür, sıfırlanmaz. Bunlar CLI belleği veya fiziksel
disk I/O değildir. Desteklenmeyen, sıfırlanan veya değişen kimlikteki sayaç `null`
kalır. I/O cihazlar üzerinde toplanır; stacked cihazlar çift sayabilir, pencere
sonrasında diske aktarılan kirli sayfalar bu pencerenin sayımına girmez.
Uzak Docker, Docker Desktop VM'si ve Windows'ta bu Linux sayaçlarının
hosttan erişilememesi sıfır kullanım anlamına gelmez.

Smoke sürücüsü statik derleme destekli `cc` ister; özel home-altı geçici alanda
public bir C fixture üretir. Her tekrarda yeni, ağsız, salt-okunur, capability'siz
container kullanır (1 CPU, 128 MiB, 64 PID, UID 65534). İmaj çekmez, model çağırmaz;
yalnız kendi rastgele ad/etiket kimliği doğrulanan container'larını temizler.
Bu sentetik yük harness/model başarısı veya rakiplere üstünlük kanıtı değildir.

### CLI sürecinin gerçek kaynakları

```text
melissa --json resources
```

Bu salt okunur komut home/sağlayıcı ayarı veya API anahtarı olmadan çalışır.
Windows, Linux ve macOS'ta geçerli CLI sürecinin RSS, süreç ömrü tepe RSS ve
kümülatif user+system CPU süresini (`cpu_time_us`) örnekler. Görev metriklerinde
başlangıç/son örnek farkı `cpu_time_delta_us` olarak saklanır. Eksik/eski sayaç
`null` kalır; bilinmeyen kullanım sıfır sayılmaz. Paralel CPU süresi duvar
süresini aşabilir; CPU yüzdesi hesaplanmaz. Tepe bellek görev öncesi kurulumu da
içerebilir; Unix'te exec öncesi başlatıcının belleğini de kapsayabilir.
Linux'ta `/proc/self/status` `VmHWM`, Windows'ta yeni sürecin peak working set
değeri ayrıca `post_exec_peak_working_set_bytes` olarak verilir; macOS'ta bu
ayrı sayaç henüz yoktur (`null`). Worker/alt süreçler bu ölçüme dahil değildir.

Windows aktarım sayaçları ve Linux `/proc/self/io` içindeki `rchar`/`wchar`
fiziksel disk I/O ölçümü değildir. macOS'ta aktarım sayaçları henüz yoktur ve
`null` döner. Bu gözlemler görev/worker kaynak sınırlarını değiştirmez.

Linux/macOS'ta API veya Docker çalıştırmadan tekrarlanabilir başlangıç profili:

```sh
cargo build --locked --release -p melissa-cli
python3 tools/profile_cli.py --binary target/release/melissa --repetitions 10 > profile.json
```

Python 3.10+ gerekir. Her tekrar yeni süreç ve ayrı geçici home kullanır;
`version`, `resources`, `init` ve `profiles` ölçülür. Çocuk süreç başına
`wait4` CPU/RSS, duvar süresi ve süreç çıktıktan sonraki CLI veri klasörünün
mantıksal/tahsisli boyutu raporlanır. Binary SHA-256 ve boyutu ayrıca kaydedilir.
CPU Python sürücüsünü içermez; süreç ömrü tepe RSS exec öncesi başlatıcı
belleğini içerebilir. `resources` vakası CLI'nin kendi RSS ve desteklendiğinde
exec sonrası tepe RSS örneğini ayrı kaydeder. Duvar süresi başlatma/ölçüm yükünü ve 1 ms
çıkış yoklama çözünürlüğünü içerir. Dosya sistemi önbelleği temizlenmez. Disk
anlık boyutu I/O veya tepe kullanım değildir; binary/build bağımlılıkları ve
Docker imajları CLI veri klasörü toplamına dahil değildir. Bu bir interaktif
oturum, model görevi veya rakip karşılaştırması değildir.

Etkileşimli terminalde `/status` model, plan/çalışma durumu, son kayıtlı isteğin
bağlam gözlemlerini ve aynı CLI sürecinin kaynaklarını gösterir. Etkin iş
sırasında önceki konuşma ölçümleri güncel token/maliyet sayımı diye sunulmaz.
Ekran model çağırmaz, talimat kuyruğuna girmez ve yetki oluşturmaz.

Bağlam kapasitesi/çıktı rezervi token, native JSON boyutu bayt cinsindendir.
Opsiyonel hash-sabitlenmiş tokenizer yalnız JSON metnini sayar; sağlayıcı chat
şablonu/görsel genişlemesi olmadığı için gerçek girdi tokeni veya kapasite/maliyet
yetkisi sayılmaz. Sağlayıcının son yanıtla bildirdiği girdi sayımı ayrı tutulur;
sonraki isteğin tahmini değildir.
Sohbette opsiyonel tokenizer kayıp/değişmişse veya ölçemiyorsa bu gözlem
bilinmiyor kalır; sohbeti engellemez ve temkinli sınırlar korunur. Bu tolerans
yalnız sohbet gözlemine aittir; görev/Gateway onay ve doğrulama yolu değişmez.
Yeni ölçümler mevcut temkinli sınır ve
kompaksiyon kurallarını gevşetmez. Sağlayıcı/model/endpoint değiştiğinde eski
bağlam gözlemi temizlenir; özgün konuşma kaydı korunur.

```text
melissa --json observe diagnose TASK
melissa --json task metrics TASK
```

Tanılama model, araç, ortam, politika, kabul ve belirsiz sonuçları ayırır;
kayıt/depolama, girdi ve durum çatışması da ayrıca gösterilir. Sınıflandırma
host hata türü, typed tool makbuzu ve kalıcı kullanım/işlem defterine dayanır.
Geçmiş başarısız araçlar son görev başarısıyla aynı şey değildir. Gönderilmiş
model isteği yoksa model testi `NOT_RUN` kalır. Belirsiz gönderim/etki otomatik
tekrarlanmaz. Nedeni kanıtlanamayan hata `unknown/INCONCLUSIVE` olarak kalır.
Tanılama komutunun `PASS` sonucu yalnız görünümün üretildiğini belirtir.

Başka oturumun açtığı görevde doğrulanmış canlı kontrolün sınırlı görünümü
sunulabilir; tam destek/disk/yedek için güvenli duraklama ve sahipliğin bitmesi
gerekir. Arızalı kaydı açamamak, kaydın sağlıklı olduğunu göstermez.

## Ölçümün kapsamı

Model gönderim/yerleşim kayıtları ve gerçek araç girişimleri süreleriyle
ilişkilendirilir. Görev duvar süresi ilk running ile terminal olay arasındadır;
duraklamayı da içerir. Eksik ölçüm sıfır veya başarı olarak doldurulmaz.

Windows kontrol yayınları gerçek CLI süreç çalışma kümesini, süreç ömründeki
tepe çalışma kümesini ve I/O transfer sayaçlarını gözlemler. Oturum başlangıcına
göre süre ve transfer farkları ayrıca hesaplanır. Normal bitiş/duraklama/hata
ölçümü kalıcı şifreli kanıt olarak saklanır; kesintide son canlı örnek elde
kalmış olabilir. Eski görevlerde ölçüm bulunmaması normaldir.

Tepe RAM yalnız görev içi tahsis değildir; CLI başlangıcını ve aynı süreçteki
diğer işleri içerir. Transfer I/O fiziksel disk I/O ölçümü değildir. Container
belleği ve disk aygıtı verisi bu host sayaçlarından çıkarılmaz; mevcut fiziksel
`disk_read_bytes/disk_written_bytes` alanları ölçülmediyse null kalır.

## OpenTelemetry

```text
melissa --json observe otlp TASK --out NEW_JSON
melissa --json observe otlp TASK --endpoint http://127.0.0.1:4318/v1/traces
```

Kaynağı authenticated journal olan OTLP/HTTP JSON trace görünümüdür; arka planda
otomatik ağ gönderimi veya yeni SDK bağımlılığı eklenmez. Açık operatör komutu,
yalnız loopback IP'deki Collector'ın `/v1/traces` adresine tek gönderim yapar.
DNS adı, kimlik bilgisi, proxy, yönlendirme ve otomatik tekrar kullanılmaz.
Uzak backend gerekiyorsa operatör kendi Collector'ını ayrıca yapılandırır.

Trace/span kimlikleri sabit görev/işlem referanslarından türetilir; ham görev
metni, araç argümanı, sağlayıcı/model adı, endpoint ve kaynak yolu aktarılmaz.
İstek kapanışı gözlenmemişse sahte bitiş zamanı üretilmez. Görev üst span'i
ölçülmüş journal penceresidir; yeniden yürütülmüş bir görev değildir.

HTTP 200 ve geçerli response gerekir. Kısmi span reddi `FAIL`, bozuk/belirsiz
yanıt veya taşıma kesintisi `INCONCLUSIVE` olur. Aynı veri otomatik tekrarlanmaz.
`PASS` Collector'ın kabul cevabıdır; backend saklama/sorgulama garantisi değildir.
PASS olmayan aktarım CLI çıkış kodu 7 üretir. Dosya çıktısı veya hiçbir endpoint
seçilmediğinde ağ aktarımı `NOT_RUN` olur.

Kapsam [OTLP standardı](https://opentelemetry.io/docs/specs/otlp/) ile uyumludur.
İşletim sistemi sayaçları [Windows process I/O](https://learn.microsoft.com/windows/win32/api/winnt/ns-winnt-io_counters)
ve [working set](https://learn.microsoft.com/windows/win32/memory/working-set)
kapsamlarıyla yorumlanır; süreç gözlemi sistem/worker ölçümü diye sunulmaz.

## Temizlenmiş destek paketi

```text
melissa --json observe support TASK --out NEW_DIRECTORY
```

Yeni klasöre `support.json` ve hash manifesti yazılır. Paket yalnız seçilmiş
host alanları, sayı/boolean ölçümler, hash'lenmiş kimlikler, uygulama/ayar özeti,
son checkpoint referansı ve manuel yeniden üretim yönlendirmesi içerir.
Ham kaynak, hedef, konuşma, hata metni, komut/argüman, log, endpoint,
sağlayıcı/model adı, credential dosyası veya ortam değişkeni kopyalanmaz.
Reproduce-failure mevcut doğrulama akışıdır; paket kendiliğinden çalıştırmaz.
İlk kaynak özeti özgün görev girdisidir; güncel kaynak diye yeniden etiketlenmez.

Çıktı yeni ve home/kaynak dışında olmalıdır. Aynı hedefe tekrar yazma reddedilir.
Paket gönderilmez veya yüklenmez; paylaşım operatörün ayrı eylemidir.

## Disk ve saklama önizlemesi

```text
melissa --json maintenance disk TASK
melissa --json maintenance retention-preview TASK
```

Disk görünümü dosyaların mantıksal boyunu, görev klasöründeki çalışma kopyaları
ve WAL dahil toplamı ve içerik hash'ine göre tekilleştirilmiş CAS boyunu gösterir.
Fiziksel tahsis/NTFS dedup ölçüldüğü iddia edilmez. Kalıcı referans, legal hold,
süre sonu ve aktif görev durumu saklama önizlemesinde görünür. Aktif kanıtlar,
final, checkpoint ve invocation referansları korunur. Bu CLI yalnız önizler;
hold/pin kaldırma veya otomatik silme yapmaz. Mevcut çekirdek GC ayrı host API'sidir.

## Şifreli taşınabilir görev yedeği

```text
melissa --json maintenance keygen --out PRIVATE_RECOVERY_FILE
melissa --json maintenance backup TASK --out NEW_ARCHIVE_DIR --key-file PRIVATE_RECOVERY_FILE
melissa --json maintenance restore --from ARCHIVE_DIR --out NEW_HOME --key-file PRIVATE_RECOVERY_FILE
melissa --home NEW_HOME --json task show TASK
```

Kurtarma dosyası işletim sistemi rastgele kaynağından üretilmiş **32 byte gizli
anahtardır**; parola veya API anahtarı değildir. Özel, kullanıcıya ait bir yerde
yedekten ayrı saklayın. Anahtar stdout/log/destek paketine yazılmaz; dosyasını
kaybetmek yedeğin açılamamasına neden olur. Mevcut anahtar dosyası değiştirilmez.

Çekirdeğin tutarlı native backup'ı kullanılır. Veritabanı metadata'sı dahil tüm
çıktılar XChaCha20-Poly1305 ile şifrelenir. Büyük dosyalar 1 MiB parçalar halinde
nonce ve arşiv/dosya/parça kimliğiyle doğrulanır; tam dosya hash'leri ve şifreli
manifest sayım/sıra/bütünlüğü bağlar. Manifest en son yayınlanır. Store anahtarı
kurtarma anahtarına sarılır; raw store anahtarı diske yazılmaz.

Yanlış anahtar, eksik/bozuk parça ve desteklenmeyen biçim/sürümde yeni home
yayınlanmaz. Doğrulama geçici çalışma alanında yapılır; bu alan uygulamanın kendi
geçici verisidir. Hedefler asla mevcut klasörün üzerine yazmaz. Başarılı restore
anahtarı mevcut Windows kullanıcısının DPAPI korumasına yeniden sarar.

Yedek tek görevin store'unu ve iç child kanıtlarını kapsar. Proje dosyalarını
hosta geri yazmaz; bütün home'un, proje belleği/index'in, sağlayıcı ayarlarının,
credential'ların, aktif worker'ın veya kontrol posta kutusunun yedeği değildir.
Restore edilmiş görev **kanıt olarak açılır**: şifreli ve doğrulanmış restore
sınırı run/resume'u ayar/credential okunmadan reddeder. Yeni kaynak/ayar/kapsam
incelemesiyle yeni görev oluşturulmalıdır; eski dış etki tekrar edilmez.

Dış arşiv biçimi `melissa.portable-task/v1`, native store biçimi v1'dir. Bu sürüm
aynı CLI paket sürümünü kabul eder. Başka/future CLI veya dış biçim sürümü açıkça
reddedilir; otomatik sürüm dönüşümü uydurulmaz. Native journal'ın desteklenen
eski sürüm geçişleri çekirdeğin checksum ve anahtar doğrulamasından geçer.

Skill: `performance-triage` 1.1.0 bu ayrımları ve karşılaştırılabilir ölçüm
kurallarını içerir. Diğer workflow skill'leri gibi açık import/onayla etkinleşir;
kendiliğinden yeni araç, temizlik, ağ veya harcama yetkisi vermez.
Terminal kısayolları: `/diagnose`, `/metrics`, `/disk`, `/retention-preview`,
`/support NEW_DIRECTORY`, `/otlp NEW_JSON`.
